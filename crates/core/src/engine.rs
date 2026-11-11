//! The engine: one thread with a tokio runtime that owns the reducer state, feeds it every event
//! (commands, mpv events, resolver and radio results), runs the effects and emits `Update`s.
//!
//! Update order a front end sees: one `Update::Player` right after start (the restored session,
//! paused/idle), then a `Player` snapshot only when it changed (about once per second while
//! playing, nothing while paused or idle), interleaved with `Page`, `Lyrics`, `Notice` and
//! `AuthRequired` as background work finishes. The receiver closes once the engine has stopped
//! mpv and saved, after `Command::Quit` or after every `Handle` was dropped.

use crate::api::{Api, ApiError};
use crate::media::Media;
use crate::player::mpv::{MpvEvent, MpvPlayer};
use crate::player::state::{reduce, Effect, EndReason, Event, State, Stream};
use crate::types::{Command, Lyrics, Rating, Snapshot, Status, Track, Update};
use crate::{auth, lyrics, persist, resolve};
use futures::future::{BoxFuture, FutureExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::RwLock;
use tokio::time::Instant;

/// The only way into the engine. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    tx: UnboundedSender<Command>,
}

impl Handle {
    /// Never blocks. Silently dropped once the engine has stopped.
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Windows window handle for the OS media controls; ignored elsewhere.
    pub hwnd: Option<*mut std::ffi::c_void>,
}

/// Starts the engine thread. Fails only when the runtime or mpv cannot be created.
pub fn start(opts: Options) -> Result<(Handle, UnboundedReceiver<Update>), String> {
    crate::logger::init();
    // Raw pointers are not Send; the handle is only passed through to souvlaki.
    let hwnd = opts.hwnd.map(|p| p as usize);
    spawn(persist::state_path(), move |tx| {
        let mpv = new_mpv(tx.clone())?;
        let api: ApiCell = Arc::new(RwLock::new(None));
        // Taken before anyone can read, so API users wait for the first connection instead of
        // seeing `None`, while startup and playback do not.
        let startup = tx.clone();
        if let Ok(mut slot) = api.clone().try_write_owned() {
            tokio::spawn(async move {
                *slot = Some(Api::new().await);
                drop(slot);
                // A stored login may be dead; the front ends learn it here.
                if auth::logged_in() {
                    if let Some(ok) = auth::signed_in().await {
                        let _ = startup.send(Msg::Up(Update::Session(Some(ok))));
                    }
                }
            });
        }
        let events = tx.clone();
        let media = Media::new(hwnd.map(|h| h as *mut std::ffi::c_void), move |c| {
            let _ = events.send(Msg::Cmd(c));
        });
        let backend = Real {
            mpv,
            api: api.clone(),
            tx,
        };
        Ok((Box::new(backend) as Box<dyn Backend>, media, api))
    })
}

/// `None` only until the first connection finishes (or forever in tests).
type ApiCell = Arc<RwLock<Option<Api>>>;
type Tx = UnboundedSender<Msg>;

/// Internal messages: everything that wakes the loop besides front-end commands.
enum Msg {
    /// From the OS media controls.
    Cmd(Command),
    Ev(Event),
    /// Background results to forward (lyrics are checked against the current track first).
    Up(Update),
}

/// The test seam: everything with side effects outside this thread.
trait Backend {
    /// Player effects (everything except `Resolve`, `FetchRadio` and `Persist`). `Err` when
    /// mpv refused a `Load` or `AppendNext`.
    fn play(&self, fx: &Effect) -> Result<(), String>;
    /// `patient`: see `resolve::resolve`.
    fn resolve(
        &self,
        video_id: String,
        patient: bool,
    ) -> BoxFuture<'static, Result<Stream, String>>;
    fn radio(&self, video_id: String) -> BoxFuture<'static, Result<Vec<Track>, String>>;
    fn lyrics(&self, track: Track) -> BoxFuture<'static, Option<Lyrics>>;
    /// A player command has been stuck for too long.
    fn stalled(&self) -> bool {
        false
    }
    /// Replace the player with a fresh one, abandoning the stuck one.
    fn restart(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct Real {
    mpv: MpvPlayer,
    api: ApiCell,
    tx: Tx,
}

fn new_mpv(tx: Tx) -> Result<MpvPlayer, String> {
    MpvPlayer::new(move |e| {
        let _ = tx.send(Msg::Ev(from_mpv(e)));
    })
}

impl Backend for Real {
    fn play(&self, fx: &Effect) -> Result<(), String> {
        let m = &self.mpv;
        match fx {
            Effect::Load(s) => m.load(s),
            Effect::AppendNext(s) => m.append_next(s),
            Effect::ClearNext => m.clear_next(),
            Effect::SetPause(p) => m.set_pause(*p),
            Effect::Seek(secs) => m.seek(*secs),
            Effect::SetVolume(v) => m.set_volume(*v),
            Effect::SetNormalize(on) => m.set_normalize(*on),
            Effect::Stop => m.stop(),
            Effect::Resolve(_) | Effect::Forget(_) | Effect::FetchRadio(_) | Effect::Persist => {}
        }
        Ok(())
    }

    fn stalled(&self) -> bool {
        self.mpv.stalled()
    }

    fn restart(&mut self) -> Result<(), String> {
        let new = new_mpv(self.tx.clone())?;
        std::mem::replace(&mut self.mpv, new).abandon();
        Ok(())
    }

    fn resolve(
        &self,
        video_id: String,
        patient: bool,
    ) -> BoxFuture<'static, Result<Stream, String>> {
        async move {
            let cookies = auth::logged_in().then(auth::cookie_path);
            resolve::resolve(&video_id, cookies.as_deref(), patient).await
        }
        .boxed()
    }

    fn radio(&self, video_id: String) -> BoxFuture<'static, Result<Vec<Track>, String>> {
        let api = self.api.clone();
        async move { with_api(&api, async |a| a.radio(&video_id).await).await }
            .map(|r| r.map_err(|e| e.to_string()))
            .boxed()
    }

    fn lyrics(&self, track: Track) -> BoxFuture<'static, Option<Lyrics>> {
        let api = self.api.clone();
        async move {
            if let Some(l) = lyrics::fetch(&track).await {
                return Some(l);
            }
            match with_api(&api, async |a| a.lyrics(&track.video_id).await).await {
                Ok(text) => text.map(|t| lyrics::plain(&t)),
                Err(e) => {
                    log::debug!("lyrics for {}: {e}", track.video_id);
                    None
                }
            }
        }
        .boxed()
    }
}

/// Variant name of a Debug value, without its payload (which may hold URLs or cookies).
fn head(v: &impl std::fmt::Debug) -> String {
    let d = format!("{v:?}");
    d.chars().take_while(|c| c.is_alphanumeric()).collect()
}

/// `host itag=N` of a stream URL; never the query string itself.
fn url_info(url: &str) -> String {
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next());
    let itag = url
        .split_once('?')
        .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("itag=")));
    format!("{} itag={}", host.unwrap_or("?"), itag.unwrap_or("?"))
}

fn not_connected() -> ApiError {
    ApiError::Other("YouTube Music is not connected".into())
}

/// Runs `f` against the API: waits for the first connection, reconnects first when it is down
/// (started offline), and gives up after `api::TIMEOUT`.
async fn with_api<T>(
    api: &ApiCell,
    f: impl AsyncFnOnce(&Api) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    if api.read().await.as_ref().is_some_and(Api::is_down) {
        let mut g = api.write().await;
        if let Some(a) = g.as_mut().filter(|a| a.is_down()) {
            a.reload_auth().await;
        }
    }
    let g = api.read().await;
    let a = g.as_ref().ok_or_else(not_connected)?;
    tokio::time::timeout(crate::api::TIMEOUT, f(a))
        .await
        .unwrap_or_else(|_| Err(ApiError::Other("YouTube Music timed out".into())))
}

fn from_mpv(e: MpvEvent) -> Event {
    match e {
        MpvEvent::FileLoaded => Event::FileLoaded,
        MpvEvent::EndFile(r) => Event::EndFile(r),
        MpvEvent::PauseChanged(p) => Event::PauseChanged(p),
        MpvEvent::TimePos(t) => Event::TimePos(t),
        MpvEvent::Duration(d) => Event::Duration(d),
        MpvEvent::Buffering(b) => Event::Buffering(b),
        MpvEvent::Refused { append: true, .. } => Event::AppendFailed,
        MpvEvent::Refused { message, .. } => Event::EndFile(EndReason::Error(message)),
    }
}

/// Runs `setup` on the new engine thread (inside its runtime) and the loop after it.
fn spawn(
    state_path: PathBuf,
    setup: impl FnOnce(Tx) -> Result<(Box<dyn Backend>, Option<Media>, ApiCell), String>
        + Send
        + 'static,
) -> Result<(Handle, UnboundedReceiver<Update>), String> {
    let (cmd_tx, cmds) = unbounded_channel();
    let (up, up_rx) = unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("cmusic-engine".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => return drop(ready_tx.send(Err(format!("tokio runtime: {e}")))),
            };
            rt.block_on(async move {
                let (tx, msgs) = unbounded_channel();
                let (backend, media, api) = match setup(tx.clone()) {
                    Ok(parts) => parts,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                let _ = ready_tx.send(Ok(()));
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |d| d.as_nanos() as u64);
                let engine = Engine {
                    st: State::new(seed),
                    backend,
                    media,
                    api,
                    tx,
                    up,
                    path: state_path,
                    last: None,
                    save_at: None,
                    last_save: Instant::now(),
                    lyrics_for: None,
                    watchdog_off: false,
                    cache: Default::default(),
                    patient: Default::default(),
                    jobs: Default::default(),
                    warm: Default::default(),
                };
                engine.run(cmds, msgs).await;
            });
            // Do not wait for a cookie import still running on the blocking pool.
            rt.shutdown_background();
        })
        .map_err(|e| format!("engine thread: {e}"))?;
    ready_rx
        .recv()
        .map_err(|_| "engine thread died during startup".to_string())??;
    Ok((Handle { tx: cmd_tx }, up_rx))
}

struct Engine {
    st: State,
    backend: Box<dyn Backend>,
    media: Option<Media>,
    api: ApiCell,
    tx: Tx,
    up: UnboundedSender<Update>,
    path: PathBuf,
    /// Last snapshot sent.
    last: Option<Snapshot>,
    /// Pending debounced save.
    save_at: Option<Instant>,
    last_save: Instant,
    /// Video id the latest lyrics request is for.
    lyrics_for: Option<String>,
    /// A restart failed: stop trying.
    watchdog_off: bool,
    cache: resolve::Cache,
    /// Ids whose next resolve waits for `available_at` (their cached stream was refused).
    patient: std::collections::HashSet<String>,
    /// Running yt-dlp resolves.
    jobs: std::collections::HashMap<String, Job>,
    /// Ids to warm when a slot is free, in order.
    warm: std::collections::VecDeque<String>,
}

struct Job {
    /// Speculative: runs only in a free slot and is the first to be killed.
    warm: bool,
    task: tokio::task::JoinHandle<()>,
}

/// Concurrent yt-dlp processes: the current track, the next one, nothing more.
const MAX_JOBS: usize = 2;
/// Rows of a fresh page worth warming.
const WARM_MAX: usize = 3;

const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const SAVE_WHILE_PLAYING: Duration = Duration::from_secs(15);

impl Engine {
    async fn run(mut self, mut cmds: UnboundedReceiver<Command>, mut msgs: UnboundedReceiver<Msg>) {
        if let Some(s) = persist::load_from(&self.path) {
            self.event(Event::Restored {
                queue: s.queue,
                cursor: s.cursor,
                position_secs: s.position_secs,
                volume: s.volume,
                shuffle: s.shuffle,
                repeat: s.repeat,
                normalize: s.normalize,
            });
        }
        self.emit();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            let save_at = self.save_at;
            let msg = tokio::select! {
                // Every Handle dropped counts as Quit.
                c = cmds.recv() => Msg::Cmd(c.unwrap_or(Command::Quit)),
                Some(m) = msgs.recv() => m,
                _ = tick.tick() => {
                    self.watchdog();
                    continue;
                }
                _ = tokio::time::sleep_until(save_at.unwrap_or_else(Instant::now)),
                    if save_at.is_some() => {
                    self.save();
                    continue;
                }
            };
            match msg {
                Msg::Cmd(Command::Quit) => break,
                Msg::Cmd(c) => self.command(c),
                Msg::Ev(e) => self.event(e),
                Msg::Up(Update::Lyrics { video_id, .. })
                    if self.lyrics_for.as_ref() != Some(&video_id) => {}
                Msg::Up(u) => {
                    let _ = self.up.send(u);
                }
            }
        }
        let _ = self.backend.play(&Effect::Stop);
        self.save();
    }

    fn command(&mut self, c: Command) {
        log::debug!("command {}", head(&c));
        match c {
            Command::Fetch(_)
            | Command::Rate { .. }
            | Command::AddToPlaylist { .. }
            | Command::RemoveFromPlaylist { .. }
            | Command::ImportCookies(_) => {
                let (api, tx) = (self.api.clone(), self.tx.clone());
                tokio::spawn(async move {
                    for u in account(&api, c).await {
                        let _ = tx.send(Msg::Up(u));
                    }
                });
            }
            Command::Warm(ids) => {
                self.warm = ids.into_iter().take(WARM_MAX).collect();
                self.pump();
            }
            c => {
                if matches!(c, Command::PlayTracks { .. }) {
                    self.warm.clear();
                }
                self.event(Event::Command(c))
            }
        }
    }

    fn event(&mut self, e: Event) {
        match &e {
            Event::Resolved { video_id, stream } => {
                self.jobs.remove(video_id);
                self.cache
                    .put(video_id, stream.clone(), resolve::unix_now());
            }
            Event::ResolveFailed { video_id, .. } => {
                self.jobs.remove(video_id);
            }
            _ => {}
        }
        let fx = reduce(&mut self.st, e);
        // Kill resolves nobody waits for any more (rapid Next presses).
        let wanted = self.st.wanted();
        self.jobs.retain(|id, j| {
            let keep = j.warm || wanted.contains(id);
            if !keep {
                j.task.abort(); // kill_on_drop ends yt-dlp
            }
            keep
        });
        // Before the effects, so the UI follows the reducer even when an effect is slow.
        self.emit();
        for fx in fx {
            self.effect(fx);
        }
        self.emit();
        self.pump();
    }

    /// Starts queued warm-ups while a slot is free.
    fn pump(&mut self) {
        while self.jobs.len() < MAX_JOBS {
            let Some(id) = self.warm.pop_front() else {
                break;
            };
            if !self.jobs.contains_key(&id) && self.cache.get(&id, resolve::unix_now()).is_none() {
                self.start_job(id, true);
            }
        }
    }

    fn start_job(&mut self, video_id: String, warm: bool) {
        // The user's track always gets a slot: the youngest warm-up makes room.
        while !warm && self.jobs.len() >= MAX_JOBS {
            let Some(id) = self
                .jobs
                .iter()
                .find(|(_, j)| j.warm)
                .map(|(id, _)| id.clone())
            else {
                break; // ponytail: user resolves are not capped; current + next never exceed 2
            };
            if let Some(j) = self.jobs.remove(&id) {
                j.task.abort();
            }
        }
        let patient = self.patient.remove(&video_id);
        let fut = self.backend.resolve(video_id.clone(), patient);
        let tx = self.tx.clone();
        let id = video_id.clone();
        let task = tokio::spawn(async move {
            let video_id = id;
            let t = std::time::Instant::now();
            log::debug!("resolve start {video_id}");
            let ev = match fut.await {
                Ok(stream) => {
                    log::debug!(
                        "resolve end {video_id} ok {}ms {}",
                        t.elapsed().as_millis(),
                        url_info(&stream.url)
                    );
                    Event::Resolved { video_id, stream }
                }
                Err(message) => {
                    log::debug!(
                        "resolve end {video_id} err {}ms: {message}",
                        t.elapsed().as_millis()
                    );
                    Event::ResolveFailed { video_id, message }
                }
            };
            let _ = tx.send(Msg::Ev(ev));
        });
        self.jobs.insert(video_id, Job { warm, task });
    }

    /// mpv stuck on a command: replace it and reload the current track where it was.
    fn watchdog(&mut self) {
        if self.watchdog_off || !self.backend.stalled() {
            return;
        }
        log::error!("mpv command stuck for over 5 s, restarting the player");
        match self.backend.restart() {
            Ok(()) => self.event(Event::PlayerRestarted),
            Err(e) => {
                log::error!("restarting the player failed: {e}");
                self.watchdog_off = true;
                self.event(Event::EndFile(EndReason::Error(format!(
                    "player stopped responding ({e})"
                ))));
            }
        }
    }

    fn effect(&mut self, fx: Effect) {
        let tx = self.tx.clone();
        log::debug!("effect {}", head(&fx));
        match fx {
            Effect::Resolve(video_id) => {
                if let Some(stream) = self.cache.get(&video_id, resolve::unix_now()) {
                    log::debug!("resolve cache hit {video_id} {}", url_info(&stream.url));
                    let _ = tx.send(Msg::Ev(Event::Resolved { video_id, stream }));
                } else if let Some(job) = self.jobs.get_mut(&video_id) {
                    job.warm = false; // already running: its answer serves this request
                } else {
                    self.start_job(video_id, false);
                }
            }
            Effect::Forget(video_id) => {
                self.cache.remove(&video_id);
                self.patient.insert(video_id);
            }
            Effect::FetchRadio(video_id) => {
                let fut = self.backend.radio(video_id.clone());
                tokio::spawn(async move {
                    let tracks = fut.await.unwrap_or_else(|e| {
                        let _ = tx.send(Msg::Up(Update::Notice(format!("Autoplay: {e}"))));
                        Vec::new()
                    });
                    let _ = tx.send(Msg::Ev(Event::RadioFetched(video_id, tracks)));
                });
            }
            Effect::Persist => {
                self.save_at
                    .get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            fx => {
                if let Err(e) = self.backend.play(&fx) {
                    log::warn!("{e}");
                    // Through the channel, after the effects of this event.
                    let ev = match fx {
                        Effect::Load(_) => Event::EndFile(EndReason::Error(e)),
                        Effect::AppendNext(_) => Event::AppendFailed,
                        _ => return,
                    };
                    let _ = tx.send(Msg::Ev(ev));
                }
            }
        }
    }

    /// Sends the snapshot if it changed, and follows up on what depends on it.
    fn emit(&mut self) {
        let snap = self.st.snapshot();
        if self.last.as_ref() == Some(&snap) {
            return;
        }
        if let Some(m) = &mut self.media {
            m.update(&snap);
        }
        let current = snap.current().cloned();
        if current.as_ref().map(|t| &t.video_id) != self.lyrics_for.as_ref() {
            self.lyrics_for = current.as_ref().map(|t| t.video_id.clone());
            if let Some(track) = current {
                let (fut, tx) = (self.backend.lyrics(track.clone()), self.tx.clone());
                tokio::spawn(async move {
                    let lyrics = fut.await;
                    let video_id = track.video_id;
                    let _ = tx.send(Msg::Up(Update::Lyrics { video_id, lyrics }));
                });
            }
        }
        if snap.status == Status::Playing
            && self.save_at.is_none()
            && self.last_save.elapsed() >= SAVE_WHILE_PLAYING
        {
            self.save_at = Some(Instant::now());
        }
        let _ = self.up.send(Update::Player(snap.clone()));
        self.last = Some(snap);
    }

    /// Inline on purpose: a few KB of JSON at most every 2 s (15 s while just playing), and a
    /// single writer means no two saves can race on the temp file.
    fn save(&mut self) {
        self.save_at = None;
        self.last_save = Instant::now();
        let saved = persist::Saved::from(&self.st.snapshot());
        if let Err(e) = persist::save_to(&self.path, &saved) {
            log::warn!("saving {}: {e}", self.path.display());
        }
    }
}

/// Runs one account/browsing command and returns the updates it produces.
async fn account(api: &ApiCell, cmd: Command) -> Vec<Update> {
    if let Command::ImportCookies(src) = cmd {
        let imported = tokio::task::spawn_blocking(move || auth::import(&src))
            .await
            .unwrap_or_else(|e| Err(format!("cookie import failed: {e}")));
        if let Err(e) = imported {
            return vec![Update::AuthRequired(e)];
        }
        let mut g = api.write().await;
        match g.as_mut() {
            Some(a) => a.reload_auth().await,
            None => *g = Some(Api::new().await),
        }
        drop(g);
        // The client always builds from a saved file; only YouTube can say if it is signed in.
        return match auth::verify().await {
            Some(true) => vec![Update::Notice("Logged in".into())],
            Some(false) => vec![
                Update::Session(Some(false)),
                Update::AuthRequired(auth::rejected_message()),
            ],
            None => vec![
                Update::Session(None),
                Update::Notice(
                    "Cookies saved, but the login could not be verified (no connection?)".into(),
                ),
            ],
        };
    }
    let (result, ok) = match cmd {
        Command::Fetch(request) => {
            let t = std::time::Instant::now();
            log::debug!("fetch start {}", head(&request));
            let result = with_api(api, async |a| a.fetch(&request).await).await;
            match &result {
                Ok(p) => log::debug!(
                    "fetch end {} ok {} items {}ms",
                    head(&request),
                    p.items.len(),
                    t.elapsed().as_millis()
                ),
                Err(e) => log::debug!(
                    "fetch end {} err {}ms: {e}",
                    head(&request),
                    t.elapsed().as_millis()
                ),
            }
            let mut out = Vec::new();
            if let Err(ApiError::Auth(m)) = &result {
                out.push(Update::AuthRequired(m.clone()));
            }
            let result = result.map_err(|e| e.to_string());
            out.insert(0, Update::Page { request, result });
            return out;
        }
        Command::Rate { video_id, rating } => (
            with_api(api, async |a| a.rate(&video_id, rating).await).await,
            match rating {
                Rating::Like => "Liked",
                Rating::Dislike => "Disliked",
                Rating::None => "Rating removed",
            },
        ),
        Command::AddToPlaylist {
            playlist_id,
            video_id,
        } => (
            with_api(api, async |a| {
                a.add_to_playlist(&playlist_id, &video_id).await
            })
            .await,
            "Added to playlist",
        ),
        Command::RemoveFromPlaylist {
            playlist_id,
            video_id,
        } => (
            with_api(api, async |a| {
                a.remove_from_playlist(&playlist_id, &video_id).await
            })
            .await,
            "Removed from playlist",
        ),
        _ => return Vec::new(),
    };
    vec![match result {
        Ok(()) => Update::Notice(ok.into()),
        Err(ApiError::Auth(m)) => Update::AuthRequired(m),
        Err(ApiError::Other(m)) => Update::Notice(m),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<String>>>;

    /// Resolves instantly to `Stream { url: id }` except for `fail`; records player calls.
    /// mpv refuses to load ids starting with `!`.
    struct Fake {
        log: Log,
        fail: &'static str,
        /// Set by any `Seek`, as a real mpv stuck in the command would report; cleared by a restart.
        stuck: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Backend for Fake {
        fn play(&self, fx: &Effect) -> Result<(), String> {
            let (line, refused) = match fx {
                Effect::Load(s) => (format!("load {}", s.url), s.url.starts_with('!')),
                Effect::AppendNext(s) => (format!("append {}", s.url), s.url.starts_with('!')),
                fx => (format!("{fx:?}"), false),
            };
            if matches!(fx, Effect::Seek(_)) {
                self.stuck.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.log.lock().unwrap().push(line);
            if refused {
                return Err("refused".into());
            }
            Ok(())
        }
        fn resolve(&self, id: String, patient: bool) -> BoxFuture<'static, Result<Stream, String>> {
            self.log.lock().unwrap().push(format!(
                "resolve {id}{}",
                if patient { " patient" } else { "" }
            ));
            let fail = id == self.fail;
            async move {
                if fail {
                    Err(format!("{id} unavailable"))
                } else {
                    Ok(Stream {
                        url: id,
                        headers: Vec::new(),
                    })
                }
            }
            .boxed()
        }
        fn stalled(&self) -> bool {
            self.stuck.load(std::sync::atomic::Ordering::Relaxed)
        }
        fn restart(&mut self) -> Result<(), String> {
            self.stuck
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.log.lock().unwrap().push("restart".into());
            Ok(())
        }
        fn radio(&self, _: String) -> BoxFuture<'static, Result<Vec<Track>, String>> {
            async { Ok(Vec::new()) }.boxed()
        }
        fn lyrics(&self, _: Track) -> BoxFuture<'static, Option<Lyrics>> {
            async { None }.boxed()
        }
    }

    struct Rig {
        handle: Handle,
        rx: UnboundedReceiver<Update>,
        inject: Tx,
        log: Log,
        dir: PathBuf,
    }

    fn rig(name: &str, fail: &'static str) -> Rig {
        let dir = std::env::temp_dir().join(format!("cmusic-engine-{}-{name}", std::process::id()));
        let log = Log::default();
        let (inject_tx, inject_rx) = std::sync::mpsc::channel();
        let fake = Fake {
            log: log.clone(),
            fail,
            stuck: Default::default(),
        };
        let (handle, rx) = spawn(dir.join("state.json"), move |tx| {
            inject_tx.send(tx).unwrap();
            Ok((
                Box::new(fake) as Box<dyn Backend>,
                None,
                Arc::new(RwLock::new(None)),
            ))
        })
        .unwrap();
        let inject = inject_rx.recv().unwrap();
        Rig {
            handle,
            rx,
            inject,
            log,
            dir,
        }
    }

    impl Rig {
        fn ev(&self, e: Event) {
            self.inject.send(Msg::Ev(e)).unwrap();
        }

        async fn wait_for(&self, line: &str) {
            for _ in 0..200 {
                if self.log.lock().unwrap().iter().any(|l| l == line) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("no {line:?} in {:?}", self.log.lock().unwrap());
        }

        /// Quits after the injected events (same channel, so in order) and returns every
        /// player snapshot sent.
        async fn finish(mut self) -> Vec<Snapshot> {
            let _ = self.inject.send(Msg::Cmd(Command::Quit));
            let mut snaps = Vec::new();
            while let Some(u) = tokio::time::timeout(Duration::from_secs(5), self.rx.recv())
                .await
                .expect("receiver closes after Quit")
            {
                if let Update::Player(s) = u {
                    snaps.push(s);
                }
            }
            std::fs::remove_dir_all(&self.dir).ok();
            snaps
        }
    }

    fn play(ids: &[&str]) -> Command {
        let tracks = ids
            .iter()
            .map(|id| Track {
                video_id: id.to_string(),
                title: id.to_string(),
                artists: String::new(),
                album: None,
                duration_secs: Some(100),
                thumbnail_url: None,
            })
            .collect();
        Command::PlayTracks { tracks, start: 0 }
    }

    #[tokio::test]
    async fn plays_three_tracks_gaplessly() {
        let r = rig("three", "");
        r.handle.send(play(&["a", "b", "c"]));
        r.wait_for("load a").await;
        r.ev(Event::FileLoaded);
        r.wait_for("append b").await;
        r.ev(Event::EndFile(EndReason::Eof));
        r.ev(Event::FileLoaded);
        r.wait_for("append c").await;
        r.ev(Event::EndFile(EndReason::Eof));
        r.ev(Event::FileLoaded);
        let log = r.log.clone();
        let snaps = r.finish().await;
        let playing: Vec<_> = snaps
            .iter()
            .filter(|s| s.status == Status::Playing)
            .map(|s| s.cursor)
            .collect();
        assert_eq!(playing, [Some(0), Some(1), Some(2)]);
        let log = log.lock().unwrap();
        let loads: Vec<_> = log.iter().filter(|l| l.starts_with("load")).collect();
        assert_eq!(loads, ["load a"]);
    }

    fn count(log: &Log, line: &str) -> usize {
        log.lock().unwrap().iter().filter(|l| *l == line).count()
    }

    #[tokio::test]
    async fn warm_cache_hit_and_manual_next_resolve_once() {
        let r = rig("warm", "");
        r.handle.send(Command::Warm(vec!["a".into(), "b".into()]));
        r.wait_for("resolve a").await;
        r.wait_for("resolve b").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        r.handle.send(play(&["a", "b", "c"]));
        r.wait_for("load a").await;
        r.ev(Event::FileLoaded);
        r.wait_for("append b").await;
        // Manual Next: b was resolved and appended, now it is a cache hit.
        r.handle.send(Command::Next);
        r.wait_for("load b").await;
        r.ev(Event::FileLoaded);
        r.wait_for("append c").await;
        let log = r.log.clone();
        r.finish().await;
        for id in ["a", "b"] {
            assert_eq!(count(&log, &format!("resolve {id}")), 1, "{id}");
        }
        assert_eq!(count(&log, "resolve c"), 1);
    }

    #[tokio::test]
    async fn refused_stream_is_dropped_and_resolved_patiently_once() {
        let r = rig("retry", "");
        r.handle.send(play(&["a", "b"]));
        r.wait_for("load a").await;
        r.ev(Event::EndFile(EndReason::Error("403".into())));
        r.wait_for("resolve a patient").await;
        r.ev(Event::FileLoaded);
        let log = r.log.clone();
        let snaps = r.finish().await;
        assert_eq!(count(&log, "load a"), 2);
        assert_eq!(snaps.last().unwrap().status, Status::Playing);
    }

    #[tokio::test]
    async fn failed_resolve_skips_the_track() {
        let r = rig("fail", "b");
        r.handle.send(play(&["a", "b", "c"]));
        r.wait_for("load a").await;
        r.ev(Event::FileLoaded);
        r.ev(Event::EndFile(EndReason::Eof));
        r.wait_for("load c").await;
        let log = r.log.clone();
        let snaps = r.finish().await;
        assert!(snaps
            .iter()
            .any(|s| s.cursor == Some(2) && s.last_error.as_deref() == Some("b unavailable")));
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|l| l == "load b" || l == "append b"));
    }

    #[tokio::test]
    async fn refused_loads_reach_the_reducer() {
        let r = rig("refused", "");
        r.handle.send(play(&["a", "!b", "c"]));
        r.wait_for("load a").await;
        r.ev(Event::FileLoaded);
        r.wait_for("append !b").await;
        // The append was refused, so end of track takes the normal path and loads it...
        r.ev(Event::EndFile(EndReason::Eof));
        r.wait_for("load !b").await;
        // ...whose refusal skips to the next track instead of waiting in Loading.
        r.wait_for("load c").await;
        let snaps = r.finish().await;
        assert!(snaps
            .iter()
            .any(|s| s.cursor == Some(2) && s.last_error.as_deref() == Some("refused")));
    }

    #[tokio::test]
    async fn stuck_seek_does_not_freeze_and_restarts_the_player() {
        let r = rig("stuck", "");
        r.handle.send(play(&["a", "b"]));
        r.wait_for("load a").await;
        r.ev(Event::FileLoaded);
        r.wait_for("append b").await;
        r.ev(Event::TimePos(40.0));
        // Through the same channel as the events, to keep the order.
        r.inject.send(Msg::Cmd(Command::SeekTo(100.0))).unwrap();
        r.wait_for("Seek(100.0)").await;
        // The seek never completes, yet commands and events keep being processed...
        r.inject.send(Msg::Cmd(Command::SetVolume(33))).unwrap();
        r.wait_for("SetVolume(33)").await;
        // ...and the watchdog replaces the player and reloads the track where it was.
        r.wait_for("restart").await;
        r.wait_for("SetVolume(33)").await;
        for _ in 0..200 {
            if r.log
                .lock()
                .unwrap()
                .iter()
                .filter(|l| *l == "load a")
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        r.ev(Event::FileLoaded);
        let snaps = r.finish().await;
        assert!(snaps
            .iter()
            .any(|s| s.volume == 33 && s.position_secs == 100.0));
        assert!(snaps.iter().any(|s| s.status == Status::Loading
            && s.position_secs == 100.0
            && s.last_error
                .as_deref()
                .is_some_and(|e| e.contains("restarting"))));
        let last = snaps.last().unwrap();
        assert_eq!(
            (last.status, last.last_error.as_deref()),
            (Status::Playing, None)
        );
    }

    #[tokio::test]
    async fn quit_or_dropping_handles_closes_updates() {
        let r = rig("quit", "");
        r.handle.send(Command::Quit);
        let snaps = r.finish().await;
        assert_eq!(snaps.len(), 1, "only the initial snapshot");
        let mut r = rig("drop", "");
        drop(r.handle);
        let closed = async { while r.rx.recv().await.is_some() {} };
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("receiver closes when every Handle is gone");
        // The final save happened too.
        assert!(persist::load_from(&r.dir.join("state.json")).is_some());
        std::fs::remove_dir_all(&r.dir).ok();
    }
}
