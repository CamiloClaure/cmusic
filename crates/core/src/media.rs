//! OS media controls (MPRIS on Linux, SMTC on Windows, Now Playing on macOS) through souvlaki.
//!
//! Threading: on Linux and Windows `Media` is `Send` (checked by a test on this platform), so the
//! engine may own it on its own thread; souvlaki runs its own service thread on Linux.
//! ponytail: on macOS souvlaki needs the main thread run loop; create `Media` there and keep the
//! loop running. Not solved here. On Windows the window's message queue must be pumped by the GUI.

use crate::{Command, Snapshot, Status};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Media {
    controls: MediaControls,
    tracker: Tracker,
    warned: bool,
    #[cfg(target_os = "linux")]
    volume: Option<u8>,
    /// Whether playback is (or is about to be) running, for the explicit Play and Pause events.
    playing: Arc<AtomicBool>,
}

impl Media {
    /// `hwnd`: Windows window handle, required there; ignored elsewhere. Returns `None` when the
    /// platform cannot provide controls (no session bus, no hwnd on Windows): the app runs without.
    pub fn new(
        hwnd: Option<*mut c_void>,
        on_command: impl Fn(Command) + Send + 'static,
    ) -> Option<Self> {
        #[cfg(target_os = "windows")]
        hwnd?;
        // souvlaki's MPRIS backend only fails inside its service thread (a panic), so check the
        // bus ourselves to fail fast and quietly.
        #[cfg(target_os = "linux")]
        if !session_bus_available() {
            log::info!("no D-Bus session bus, OS media controls disabled");
            return None;
        }
        let config = PlatformConfig {
            display_name: "cmusic",
            dbus_name: "cmusic",
            hwnd,
        };
        let mut controls = MediaControls::new(config)
            .map_err(|e| log::warn!("media controls unavailable: {e:?}"))
            .ok()?;
        let playing = Arc::new(AtomicBool::new(false));
        let flag = playing.clone();
        controls
            .attach(move |ev| {
                if let Some(cmd) = map_event(&ev, flag.load(Ordering::Relaxed)) {
                    on_command(cmd);
                }
            })
            .map_err(|e| log::warn!("media controls unavailable: {e:?}"))
            .ok()?;
        Some(Media {
            controls,
            tracker: Tracker::default(),
            warned: false,
            #[cfg(target_os = "linux")]
            volume: None,
            playing,
        })
    }

    /// Call with every snapshot; souvlaki is only touched when something changed.
    pub fn update(&mut self, snap: &Snapshot) {
        self.playing
            .store(is_playing(snap.status), Ordering::Relaxed);
        let (meta, playback) = self.tracker.plan(snap, Instant::now());
        let mut res = Ok(());
        if meta {
            let t = snap.current();
            res = self.controls.set_metadata(MediaMetadata {
                title: t.map(|t| t.title.as_str()),
                artist: t.map(|t| t.artists.as_str()),
                album: t.and_then(|t| t.album.as_deref()),
                cover_url: t.and_then(|t| t.thumbnail_url.as_deref()),
                duration: t.map(|_| Duration::from_secs_f64(snap.duration_secs.max(0.0))),
            });
        }
        if res.is_ok() && playback {
            res = self.controls.set_playback(playback_of(snap));
        }
        // MPRIS keeps its own volume property, so mirror ours when it changes.
        #[cfg(target_os = "linux")]
        if res.is_ok() && self.volume != Some(snap.volume) {
            self.volume = Some(snap.volume);
            res = self.controls.set_volume(f64::from(snap.volume) / 100.0);
        }
        if let Err(e) = res {
            if !self.warned {
                self.warned = true;
                log::warn!("media controls update failed: {e:?}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn session_bus_available() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty())
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|d| std::path::Path::new(&d).join("bus").exists())
}

fn is_playing(s: Status) -> bool {
    matches!(s, Status::Playing | Status::Loading | Status::Buffering)
}

fn playback_of(snap: &Snapshot) -> MediaPlayback {
    let progress = Some(MediaPosition(Duration::from_secs_f64(
        snap.position_secs.max(0.0),
    )));
    match snap.status {
        Status::Idle | Status::Error => MediaPlayback::Stopped,
        Status::Paused => MediaPlayback::Paused { progress },
        _ => MediaPlayback::Playing { progress },
    }
}

fn map_event(ev: &MediaControlEvent, playing: bool) -> Option<Command> {
    let dir = |d: &SeekDirection| {
        if *d == SeekDirection::Forward {
            1.0
        } else {
            -1.0
        }
    };
    Some(match ev {
        MediaControlEvent::Toggle => Command::TogglePause,
        MediaControlEvent::Play if !playing => Command::TogglePause,
        MediaControlEvent::Pause if playing => Command::TogglePause,
        MediaControlEvent::Next => Command::Next,
        MediaControlEvent::Previous => Command::Prev,
        // ponytail: "undetermined amount" seeks use a fixed 5 s step.
        MediaControlEvent::Seek(d) => Command::SeekBy(5.0 * dir(d)),
        MediaControlEvent::SeekBy(d, by) => Command::SeekBy(by.as_secs_f64() * dir(d)),
        MediaControlEvent::SetPosition(p) => Command::SeekTo(p.0.as_secs_f64()),
        MediaControlEvent::SetVolume(v) => {
            Command::SetVolume((v * 100.0).round().clamp(0.0, 100.0) as u8)
        }
        _ => return None,
    })
}

/// What was last pushed to the OS, so unchanged snapshots cost nothing.
#[derive(Default)]
struct Tracker {
    /// (video id, duration in ms) of the pushed metadata.
    meta: Option<(String, u64)>,
    /// Pushed status, position and when.
    playback: Option<(Status, f64, Instant)>,
}

impl Tracker {
    /// Returns (push metadata, push playback) and records what will be pushed.
    fn plan(&mut self, snap: &Snapshot, now: Instant) -> (bool, bool) {
        let key = snap
            .current()
            .map(|t| (t.video_id.clone(), (snap.duration_secs * 1000.0) as u64));
        let meta = key != self.meta && !(key.is_none() && self.meta.is_none());
        if meta {
            self.meta = key;
        }
        let playback = meta
            || match self.playback {
                None => true,
                Some((status, pos, at)) => {
                    let predicted = if status == Status::Playing {
                        pos + now.saturating_duration_since(at).as_secs_f64()
                    } else {
                        pos
                    };
                    status != snap.status || (snap.position_secs - predicted).abs() > 2.0
                }
            };
        if playback {
            self.playback = Some((snap.status, snap.position_secs, now));
        }
        (meta, playback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;

    fn snap(id: &str, status: Status, pos: f64) -> Snapshot {
        Snapshot {
            status,
            queue: vec![Track {
                video_id: id.into(),
                title: "t".into(),
                artists: "a".into(),
                album: None,
                duration_secs: Some(200),
                thumbnail_url: None,
            }],
            cursor: Some(0),
            position_secs: pos,
            duration_secs: 200.0,
            ..Default::default()
        }
    }

    #[test]
    fn pushes_only_on_change() {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let s = |secs: u64| t0 + Duration::from_secs(secs);
        assert_eq!(t.plan(&snap("a", Status::Playing, 0.0), s(0)), (true, true));
        // normal one-second ticks: nothing to send
        assert_eq!(
            t.plan(&snap("a", Status::Playing, 1.0), s(1)),
            (false, false)
        );
        assert_eq!(
            t.plan(&snap("a", Status::Playing, 2.0), s(2)),
            (false, false)
        );
        // seek
        assert_eq!(
            t.plan(&snap("a", Status::Playing, 100.0), s(3)),
            (false, true)
        );
        assert_eq!(
            t.plan(&snap("a", Status::Playing, 101.0), s(4)),
            (false, false)
        );
        // pause, then paused ticks without position drift
        assert_eq!(
            t.plan(&snap("a", Status::Paused, 101.0), s(5)),
            (false, true)
        );
        assert_eq!(
            t.plan(&snap("a", Status::Paused, 101.0), s(9)),
            (false, false)
        );
        // new track
        assert_eq!(
            t.plan(&snap("b", Status::Playing, 0.0), s(10)),
            (true, true)
        );
        // same id, duration became known later
        let mut late = snap("b", Status::Playing, 1.0);
        late.duration_secs = 201.5;
        assert_eq!(t.plan(&late, s(11)), (true, true));
        // nothing playing: one clear, then silence
        let idle = Snapshot::default();
        assert_eq!(t.plan(&idle, s(12)), (true, true));
        assert_eq!(t.plan(&idle, s(13)), (false, false));
    }

    #[test]
    fn idle_start_sends_playback_once() {
        let mut t = Tracker::default();
        let now = Instant::now();
        assert_eq!(t.plan(&Snapshot::default(), now), (false, true));
        assert_eq!(t.plan(&Snapshot::default(), now), (false, false));
    }

    #[test]
    fn maps_events() {
        use MediaControlEvent as E;
        assert_eq!(map_event(&E::Play, false), Some(Command::TogglePause));
        assert_eq!(map_event(&E::Play, true), None);
        assert_eq!(map_event(&E::Pause, true), Some(Command::TogglePause));
        assert_eq!(map_event(&E::Pause, false), None);
        assert_eq!(map_event(&E::Previous, true), Some(Command::Prev));
        assert_eq!(
            map_event(&E::SetVolume(0.5), true),
            Some(Command::SetVolume(50))
        );
        assert_eq!(
            map_event(&E::SetVolume(7.0), true),
            Some(Command::SetVolume(100))
        );
        assert_eq!(
            map_event(
                &E::SetPosition(MediaPosition(Duration::from_secs(30))),
                true
            ),
            Some(Command::SeekTo(30.0))
        );
        assert_eq!(map_event(&E::Stop, true), None);
    }

    #[test]
    fn media_is_send() {
        fn send<T: Send>() {}
        send::<Media>();
    }

    #[test]
    fn new_without_bus_is_fast_and_quiet() {
        let start = Instant::now();
        let m = Media::new(None, |_| {});
        // On a box with a session bus this is Some; either way it must not hang or panic.
        println!(
            "Media::new -> {}",
            if m.is_some() { "Some" } else { "None" }
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
