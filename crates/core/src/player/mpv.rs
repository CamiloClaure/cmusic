//! Thin adapter over libmpv. No playback logic: methods mirror engine effects and
//! mpv's events come back through a callback from one dedicated thread.

use crate::resolve::Stream;
use libmpv2::events::{Event, PropertyData};
use libmpv2::{mpv_end_file_reason, mpv_error, Error, Format, Mpv};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    FileLoaded,
    EndFile(EndReason),
    PauseChanged(bool),
    TimePos(f64),
    Duration(f64),
    Buffering(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndReason {
    Eof,
    Error(String),
    Stopped,
}

pub struct MpvPlayer {
    mpv: Mpv,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MpvPlayer {
    pub fn new(on_event: impl Fn(MpvEvent) + Send + 'static) -> Result<Self, String> {
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

        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
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
                            on_event(MpvEvent::EndFile(EndReason::Error(describe(&e))));
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
                        Event::Shutdown => break,
                        _ => continue,
                    };
                    on_event(out);
                }
            })
            .map_err(|e| format!("mpv thread failed: {e}"))?;

        Ok(Self {
            mpv,
            stop,
            thread: Some(thread),
        })
    }

    /// Replace the playlist and start playing `s`.
    pub fn load(&self, s: &Stream) {
        self.loadfile(s, "replace");
    }

    /// Append `s` as the gapless next entry.
    pub fn append_next(&self, s: &Stream) {
        self.loadfile(s, "append");
    }

    /// Remove every playlist entry after the current one.
    pub fn clear_next(&self) {
        let (Ok(count), Ok(pos)) = (
            self.mpv.get_property::<i64>("playlist-count"),
            self.mpv.get_property::<i64>("playlist-pos"),
        ) else {
            return;
        };
        for i in (pos + 1..count).rev() {
            self.cmd("playlist-remove", &[&i.to_string()]);
        }
    }

    pub fn set_pause(&self, paused: bool) {
        if let Err(e) = self.mpv.set_property("pause", paused) {
            log::warn!("mpv pause: {e}");
        }
    }

    pub fn seek(&self, secs: f64) {
        self.cmd("seek", &[&secs.to_string(), "absolute"]);
    }

    pub fn set_volume(&self, v: u8) {
        if let Err(e) = self.mpv.set_property("volume", f64::from(v)) {
            log::warn!("mpv volume: {e}");
        }
    }

    pub fn set_normalize(&self, on: bool) {
        if let Err(e) = self
            .mpv
            .set_property("af", if on { "loudnorm" } else { "" })
        {
            log::warn!("mpv af: {e}");
        }
    }

    pub fn stop(&self) {
        self.cmd("stop", &[]);
    }

    fn cmd(&self, name: &str, args: &[&str]) {
        if let Err(e) = self.mpv.command(name, args) {
            log::warn!("mpv {name}: {e}");
        }
    }

    fn loadfile(&self, s: &Stream, mode: &str) {
        self.cmd("loadfile", &[&s.url, mode, "-1", &file_options(&s.headers)]);
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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
        Error::Raw(c) if *c == mpv_error::AoInitFailed => "audio output failed to start".into(),
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
        let s = resolve("dQw4w9WgXcQ", None).await.unwrap();
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
        let s = resolve("dQw4w9WgXcQ", None).await.unwrap();
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
        let s = resolve("dQw4w9WgXcQ", None).await.unwrap();
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
