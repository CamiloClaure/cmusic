//! Thin adapter over libmpv. No playback logic: methods mirror engine effects and
//! mpv's events come back through a callback from one dedicated thread.

pub use crate::player::state::{EndReason, Stream};
use libmpv2::events::{Event, PropertyData};
use libmpv2::{mpv_end_file_reason, mpv_error, Error, Format, Mpv};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    FileLoaded,
    EndFile(EndReason),
    PauseChanged(bool),
    TimePos(f64),
    Duration(f64),
    Buffering(bool),
    /// mpv refused a `loadfile` (`append`: the gapless next one).
    Refused {
        append: bool,
        message: String,
    },
}

/// Work for the command thread; runs in submission order.
type Job = Box<dyn FnOnce(&Mpv) + Send>;

/// A command running longer than this means mpv is stuck (the audio device, usually).
const STALL: Duration = Duration::from_secs(5);

/// Commands run on their own thread, so a blocked mpv can never block the caller.
pub struct MpvPlayer {
    jobs: Option<Sender<Job>>,
    /// Start of the running job in ms since `born`, plus 1; 0 when idle.
    busy: Arc<AtomicU64>,
    born: Instant,
    on_event: Arc<dyn Fn(MpvEvent) + Send + Sync>,
    stop: Arc<AtomicBool>,
    events: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl MpvPlayer {
    pub fn new(on_event: impl Fn(MpvEvent) + Send + Sync + 'static) -> Result<Self, String> {
        let on_event: Arc<dyn Fn(MpvEvent) + Send + Sync> = Arc::new(on_event);
        let mpv = Mpv::with_initializer(|i| {
            for (k, v) in [
                ("video", "no"),
                ("audio-display", "no"),
                ("gapless-audio", "yes"),
                ("prefetch-playlist", "yes"),
                ("stop-screensaver", "no"),
                ("ytdl", "no"),
                ("terminal", "no"),
                ("idle", "yes"),
                ("input-default-bindings", "no"),
                ("audio-client-name", "cmusic"),
                ("network-timeout", "10"),
                // Audio seeks land on the nearest cue point: no decoding up to the exact frame.
                ("hr-seek", "no"),
            ] {
                // `ytdl` only exists in builds with Lua; without it there is nothing to disable.
                match i.set_option(k, v) {
                    Err(Error::Raw(c)) if k == "ytdl" && c == mpv_error::OptionNotFound => {}
                    r => r?,
                }
            }
            if let Ok(ao) = std::env::var("CMUSIC_AO") {
                i.set_option("ao", ao.as_str())?;
            }
            Ok(())
        })
        .map_err(|e| format!("mpv init failed: {e}"))?;

        // A separate client has its own event queue, so this thread can block in
        // wait_event while other threads send commands through `mpv` (Mpv is Sync).
        let client = mpv
            .create_client(None)
            .map_err(|e| format!("mpv client failed: {e}"))?;
        for (id, name, fmt) in [
            (1, "pause", Format::Flag),
            (2, "time-pos", Format::Double),
            (3, "duration", Format::Double),
            (4, "paused-for-cache", Format::Flag),
        ] {
            client
                .observe_property(name, fmt, id)
                .map_err(|e| format!("mpv observe {name} failed: {e}"))?;
        }

        // Forward mpv's own warnings and errors to the log.
        // ponytail: libmpv2 does not wrap mpv_request_log_messages (and the sys crate is not a
        // direct dependency), so declare the one C function; libmpv is already linked.
        extern "C" {
            fn mpv_request_log_messages(
                ctx: *mut std::ffi::c_void,
                level: *const std::ffi::c_char,
            ) -> std::ffi::c_int;
        }
        // SAFETY: `ctx` is the live handle of `client`; the level is a NUL-terminated literal.
        let rc = unsafe { mpv_request_log_messages(client.ctx.as_ptr().cast(), c"warn".as_ptr()) };
        if rc < 0 {
            log::warn!("mpv log messages unavailable: error {rc}");
        }

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let cb = on_event.clone();
        let events = std::thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || {
                let mut last_sec = -1i64;
                while !flag.load(Ordering::Relaxed) {
                    let ev = match client.wait_event(0.1) {
                        None => continue,
                        Some(Ok(ev)) => ev,
                        // libmpv2 reports an end-file that carries an error as Err
                        // and drops the event, so every Err here is a failed file.
                        Some(Err(e)) => {
                            log::debug!("mpv event error: {e}");
                            let no_audio =
                                matches!(&e, Error::Raw(c) if *c == mpv_error::AoInitFailed);
                            cb(MpvEvent::EndFile(if no_audio {
                                EndReason::NoAudio
                            } else {
                                EndReason::Error(describe(&e))
                            }));
                            continue;
                        }
                    };
                    let out = match ev {
                        Event::FileLoaded => {
                            last_sec = -1;
                            MpvEvent::FileLoaded
                        }
                        Event::EndFile(r) if r == mpv_end_file_reason::Eof => {
                            MpvEvent::EndFile(EndReason::Eof)
                        }
                        Event::EndFile(_) => MpvEvent::EndFile(EndReason::Stopped),
                        Event::PropertyChange { name, change, .. } => match (name, change) {
                            ("pause", PropertyData::Flag(b)) => MpvEvent::PauseChanged(b),
                            ("paused-for-cache", PropertyData::Flag(b)) => MpvEvent::Buffering(b),
                            ("duration", PropertyData::Double(d)) => MpvEvent::Duration(d),
                            ("time-pos", PropertyData::Double(t)) => {
                                if t.floor() as i64 == last_sec {
                                    continue;
                                }
                                last_sec = t.floor() as i64;
                                MpvEvent::TimePos(t)
                            }
                            _ => continue,
                        },
                        Event::LogMessage {
                            prefix,
                            level,
                            text,
                            ..
                        } => {
                            let lvl = match level {
                                "fatal" | "error" => log::Level::Error,
                                _ => log::Level::Warn,
                            };
                            log::log!(target: "mpv", lvl, "[{prefix}] {}", text.trim_end());
                            continue;
                        }
                        Event::Shutdown => break,
                        _ => continue,
                    };
                    log::debug!("mpv event {out:?}");
                    cb(out);
                }
            })
            .map_err(|e| format!("mpv thread failed: {e}"))?;

        let (jobs, rx) = channel::<Job>();
        let busy = Arc::new(AtomicU64::new(0));
        let born = Instant::now();
        let b = busy.clone();
        let worker = std::thread::Builder::new()
            .name("mpv-commands".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    b.store(born.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
                    job(&mpv);
                    b.store(0, Ordering::Relaxed);
                }
                // `mpv` drops here, after the events thread has been joined.
            })
            .map_err(|e| format!("mpv commands thread failed: {e}"))?;

        Ok(Self {
            jobs: Some(jobs),
            busy,
            born,
            on_event,
            stop,
            events: Some(events),
            worker: Some(worker),
        })
    }

    /// True when a command has been running for longer than `STALL`.
    pub fn stalled(&self) -> bool {
        let start = self.busy.load(Ordering::Relaxed);
        let now = self.born.elapsed().as_millis() as u64 + 1;
        start != 0 && now.saturating_sub(start) > STALL.as_millis() as u64
    }

    /// Give up on this instance without waiting for it: it may never finish.
    pub fn abandon(mut self) {
        self.shutdown(Duration::ZERO);
    }

    fn shutdown(&mut self, wait: Duration) {
        self.stop.store(true, Ordering::Relaxed);
        self.jobs = None;
        let (events, worker) = (self.events.take(), self.worker.take());
        if events.is_none() {
            return;
        }
        let (tx, rx) = channel();
        // ponytail: a stuck mpv leaks this thread (and itself); there is no way to kill it.
        let _ = std::thread::Builder::new()
            .name("mpv-teardown".into())
            .spawn(move || {
                events
                    .into_iter()
                    .chain(worker)
                    .for_each(|t| drop(t.join()));
                let _ = tx.send(());
            });
        let _ = rx.recv_timeout(wait);
    }

    fn send(&self, job: impl FnOnce(&Mpv) + Send + 'static) {
        if let Some(j) = &self.jobs {
            let _ = j.send(Box::new(job));
        }
    }

    /// Replace the playlist and start playing `s`. A refusal comes back as `MpvEvent::Refused`.
    pub fn load(&self, s: &Stream) {
        self.loadfile(s, false);
    }

    /// Append `s` as the gapless next entry.
    pub fn append_next(&self, s: &Stream) {
        // After a gapless advance the finished entry is still listed: keep it to two.
        self.clear_next();
        self.loadfile(s, true);
    }

    /// Remove every playlist entry except the one playing.
    pub fn clear_next(&self) {
        self.cmd("playlist-clear", &[]);
    }

    pub fn set_pause(&self, paused: bool) {
        self.prop("pause", paused);
    }

    pub fn seek(&self, secs: f64) {
        self.cmd("seek", &[&secs.to_string(), "absolute"]);
    }

    pub fn set_volume(&self, v: u8) {
        self.prop("volume", f64::from(v));
    }

    pub fn set_normalize(&self, on: bool) {
        self.prop("af", if on { "loudnorm" } else { "" });
    }

    pub fn stop(&self) {
        self.cmd("stop", &[]);
    }

    fn cmd(&self, name: &'static str, args: &[&str]) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.send(move |m| {
            let t = Instant::now();
            log::debug!("mpv command {name} start");
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let r = m.command(name, &args);
            log::debug!("mpv command {name} end {}ms", t.elapsed().as_millis());
            if let Err(e) = r {
                log::warn!("mpv {name}: {e}");
            }
        });
    }

    fn prop<T: libmpv2::SetData + Send + 'static>(&self, name: &'static str, v: T) {
        self.send(move |m| {
            let t = Instant::now();
            log::debug!("mpv set {name} start");
            let r = m.set_property(name, v);
            log::debug!("mpv set {name} end {}ms", t.elapsed().as_millis());
            if let Err(e) = r {
                log::warn!("mpv {name}: {e}");
            }
        });
    }

    fn loadfile(&self, s: &Stream, append: bool) {
        let (url, opts) = (s.url.clone(), file_options(&s.headers));
        let cb = self.on_event.clone();
        let mode = if append { "append" } else { "replace" };
        self.send(move |m| {
            let t = Instant::now();
            log::debug!("mpv command loadfile {mode} start");
            let r = m.command("loadfile", &[&url, mode, "-1", &opts]);
            log::debug!(
                "mpv command loadfile {mode} end {}ms",
                t.elapsed().as_millis()
            );
            if let Err(e) = r {
                cb(MpvEvent::Refused {
                    append,
                    message: format!("mpv could not load the stream ({e})"),
                });
            }
        });
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.shutdown(Duration::from_secs(1));
    }
}

/// Per-file option list for `loadfile`. Values use mpv's `%len%value` form so
/// commas and `=` inside them are not split; inside the header list itself a
/// comma separates entries, so commas within a header value are escaped.
fn file_options(headers: &[(String, String)]) -> String {
    let quote = |v: &str| format!("%{}%{v}", v.len());
    let mut opts = Vec::new();
    let mut fields = Vec::new();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("user-agent") {
            opts.push(format!("user-agent={}", quote(v)));
        } else {
            fields.push(format!(
                "{k}: {}",
                v.replace('\\', "\\\\").replace(',', "\\,")
            ));
        }
    }
    if !fields.is_empty() {
        opts.push(format!("http-header-fields={}", quote(&fields.join(","))));
    }
    opts.join(",")
}

fn describe(e: &Error) -> String {
    match e {
        Error::Raw(c) if *c == mpv_error::LoadingFailed => "loading failed".into(),
        Error::Raw(c) if *c == mpv_error::UnknownFormat => "unrecognized audio format".into(),
        Error::Raw(c) if *c == mpv_error::NothingToPlay => "nothing to play".into(),
        e => format!("playback failed ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::resolve;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::{Duration, Instant};

    fn player() -> (MpvPlayer, Receiver<MpvEvent>) {
        let (tx, rx) = channel();
        let p = MpvPlayer::new(move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        (p, rx)
    }

    /// Collect events until `done` says stop or the deadline passes.
    fn collect(
        rx: &Receiver<MpvEvent>,
        secs: u64,
        mut done: impl FnMut(&[MpvEvent]) -> bool,
    ) -> Vec<MpvEvent> {
        let end = Instant::now() + Duration::from_secs(secs);
        let mut v = Vec::new();
        while Instant::now() < end && !done(&v) {
            if let Ok(e) = rx.recv_timeout(Duration::from_millis(200)) {
                v.push(e);
            }
        }
        v
    }

    #[test]
    fn options_escape() {
        let h = vec![
            ("User-Agent".into(), "a,b=c".into()),
            ("Accept".into(), "x,y".into()),
        ];
        assert_eq!(
            file_options(&h),
            "user-agent=%5%a,b=c,http-header-fields=%12%Accept: x\\,y"
        );
    }

    #[tokio::test]
    #[ignore = "needs network, yt-dlp and libmpv"]
    async fn plays_real_stream() {
        let s = resolve("dQw4w9WgXcQ", None, false).await.unwrap();
        let (p, rx) = player();
        p.load(&s);
        let evs = collect(&rx, 30, |v| {
            v.iter()
                .filter(|e| matches!(e, MpvEvent::TimePos(_)))
                .count()
                >= 2
        });
        println!("{evs:?}");
        assert!(evs.contains(&MpvEvent::FileLoaded));
        assert!(
            evs.iter()
                .filter(|e| matches!(e, MpvEvent::TimePos(_)))
                .count()
                >= 2
        );
    }

    #[tokio::test]
    #[ignore = "needs network, yt-dlp and libmpv"]
    async fn gapless_transition() {
        let s = resolve("dQw4w9WgXcQ", None, false).await.unwrap();
        let (p, rx) = player();
        p.load(&s);
        p.append_next(&s);
        let evs = collect(&rx, 30, |v| {
            v.iter().any(|e| matches!(e, MpvEvent::Duration(_)))
        });
        let dur = evs
            .iter()
            .find_map(|e| {
                if let MpvEvent::Duration(d) = e {
                    Some(*d)
                } else {
                    None
                }
            })
            .expect("duration event");
        p.seek(dur - 3.0);
        let evs = collect(&rx, 40, |v| {
            let at = v
                .iter()
                .position(|e| *e == MpvEvent::EndFile(EndReason::Eof));
            at.is_some_and(|i| v[i..].iter().any(|e| matches!(e, MpvEvent::TimePos(_))))
        });
        println!("{evs:?}");
        let eof = evs
            .iter()
            .position(|e| *e == MpvEvent::EndFile(EndReason::Eof))
            .expect("eof");
        let loaded = eof
            + evs[eof..]
                .iter()
                .position(|e| *e == MpvEvent::FileLoaded)
                .expect("loaded");
        let pos = evs[loaded..]
            .iter()
            .find_map(|e| {
                if let MpvEvent::TimePos(t) = e {
                    Some(*t)
                } else {
                    None
                }
            })
            .expect("timepos");
        assert!(pos < 2.0, "pos {pos}");
    }

    #[tokio::test]
    #[ignore = "needs network, yt-dlp and libmpv"]
    async fn load_replaces_playing_file_as_stopped() {
        let s = resolve("dQw4w9WgXcQ", None, false).await.unwrap();
        let (p, rx) = player();
        p.load(&s);
        collect(&rx, 30, |v| v.contains(&MpvEvent::FileLoaded));
        p.load(&s);
        let evs = collect(&rx, 30, |v| v.contains(&MpvEvent::FileLoaded));
        println!("{evs:?}");
        assert!(!evs.contains(&MpvEvent::EndFile(EndReason::Eof)));
        let stopped = evs
            .iter()
            .position(|e| *e == MpvEvent::EndFile(EndReason::Stopped));
        assert!(stopped.is_some_and(|i| i < evs.len() - 1));
    }

    #[test]
    #[ignore = "needs libmpv"]
    fn bad_url_errors() {
        let (p, rx) = player();
        p.load(&Stream {
            url: "https://127.0.0.1:9/nope".into(),
            headers: vec![],
        });
        let evs = collect(&rx, 20, |v| {
            v.iter().any(|e| matches!(e, MpvEvent::EndFile(_)))
        });
        println!("{evs:?}");
        assert!(evs
            .iter()
            .any(|e| matches!(e, MpvEvent::EndFile(EndReason::Error(_)))));
    }
}
