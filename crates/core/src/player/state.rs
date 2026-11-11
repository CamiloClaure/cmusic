//! The playback state machine. `reduce` turns one event into state changes plus effects for the
//! engine to run. Pure: no IO, no clocks, no threads.
//!
//! mpv's playlist holds at most two entries, the current track and the gapless next one, and the
//! reducer always knows which: `Load` replaces everything, `AppendNext` adds the next entry,
//! `ClearNext` drops it, `Stop` empties it.

use crate::queue::Queue;
use crate::types::{Command, Repeat, Snapshot, Status, Track};

/// A playable stream: the direct URL plus the HTTP headers mpv must send with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// Why mpv finished a file (mpv `end-file` reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Played to the end.
    Eof,
    /// mpv could not open or keep playing it.
    Error(String),
    /// The audio device could not be opened. Every track would fail the same way, so there is
    /// no skipping to the next one.
    NoAudio,
    /// Replaced by `Load` or ended by `Stop`. Always ignored.
    Stopped,
}

/// Everything that can happen to the player.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A front-end command. Non-playback commands are ignored.
    Command(Command),
    /// Answer to `Effect::Resolve`.
    Resolved { video_id: String, stream: Stream },
    /// Answer to `Effect::Resolve` when it failed. `message` is shown to the user.
    ResolveFailed { video_id: String, message: String },
    /// mpv `file-loaded`.
    FileLoaded,
    /// mpv `end-file`.
    EndFile(EndReason),
    /// mpv `pause` property.
    PauseChanged(bool),
    /// mpv `time-pos`, seconds.
    TimePos(f64),
    /// mpv `duration`, seconds.
    Duration(f64),
    /// mpv `paused-for-cache`.
    Buffering(bool),
    /// Answer to `Effect::FetchRadio`: the seed video id and its tracks, possibly empty.
    RadioFetched(String, Vec<Track>),
    /// mpv refused the `AppendNext`: nothing is appended after all.
    AppendFailed,
    /// The engine replaced a stuck mpv: reload the current track at the position it had.
    PlayerRestarted,
    /// The saved session, sent once at startup. Comes back `Idle` without playing; the next
    /// `TogglePause` starts the current track and seeks to `position_secs`.
    Restored {
        /// Play order, as in `Snapshot::queue`.
        queue: Vec<Track>,
        cursor: Option<usize>,
        position_secs: f64,
        volume: u8,
        shuffle: bool,
        repeat: Repeat,
        normalize: bool,
    },
}

/// Work for the engine, to run in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Resolve this video id, answer with `Resolved` or `ResolveFailed`.
    Resolve(String),
    /// Replace mpv's whole playlist with this stream and play it.
    Load(Stream),
    /// Append to mpv's playlist as the gapless next entry.
    AppendNext(Stream),
    /// Drop the cached stream of this video id: mpv refused it, the retry must resolve afresh.
    Forget(String),
    /// Remove the appended next entry (everything after mpv's current one).
    ClearNext,
    /// Set mpv's `pause` property.
    SetPause(bool),
    /// Absolute seek, seconds.
    Seek(f64),
    /// 0..=100.
    SetVolume(u8),
    /// Loudness normalization on or off.
    SetNormalize(bool),
    /// Stop playback and empty mpv's playlist.
    Stop,
    /// Fetch radio tracks for this seed video id, answer with `RadioFetched`.
    FetchRadio(String),
    /// Save the session. Always the last effect when present.
    Persist,
}

/// What the reducer knows about mpv's second playlist entry.
#[derive(Debug, Clone, PartialEq)]
enum Next {
    Empty,
    /// `Resolve` sent for this video id.
    Resolving(String),
    /// `AppendNext` sent for this video id.
    Appended(String),
}

/// Player state: the queue plus playback fields. Feed it with `reduce`.
#[derive(Debug, Clone)]
pub struct State {
    queue: Queue,
    status: Status,
    /// `Load` was sent for the current track (or mpv moved to it gaplessly).
    load_sent: bool,
    /// Mirrors mpv's `pause` property, which survives file changes.
    paused: bool,
    next: Next,
    position: f64,
    duration: f64,
    volume: u8,
    normalize: bool,
    last_error: Option<String>,
    /// Failures since the last successful `FileLoaded`.
    failures: u8,
    /// Seed of the radio fetch in flight.
    radio_pending: Option<String>,
    /// Restored or user-requested position, applied on the next `FileLoaded`.
    resume_at: Option<f64>,
    /// The current track was already retried once after mpv refused its stream.
    retried: bool,
}

impl State {
    /// Empty, idle player at full volume. `shuffle_seed` drives the shuffle order.
    pub fn new(shuffle_seed: u64) -> Self {
        Self {
            queue: Queue::new(shuffle_seed),
            status: Status::Idle,
            load_sent: false,
            paused: false,
            next: Next::Empty,
            position: 0.0,
            duration: 0.0,
            volume: 100,
            normalize: false,
            last_error: None,
            failures: 0,
            radio_pending: None,
            resume_at: None,
            retried: false,
        }
    }

    /// Video ids whose stream the player wants resolved now: the current track and mpv's next.
    pub fn wanted(&self) -> Vec<String> {
        let cur = self.queue.current().map(|t| t.video_id.clone());
        let next = match &self.next {
            Next::Resolving(id) | Next::Appended(id) => Some(id.clone()),
            Next::Empty => None,
        };
        cur.into_iter().chain(next).collect()
    }

    /// What the front ends draw.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            status: self.status,
            queue: self.queue.tracks(),
            cursor: self.queue.cursor(),
            // Until the restored position is applied, it is the one to show and save.
            position_secs: self.resume_at.unwrap_or(self.position),
            duration_secs: self.duration,
            volume: self.volume,
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
            normalize: self.normalize,
            last_error: self.last_error.clone(),
        }
    }

    fn loaded(&self) -> bool {
        matches!(
            self.status,
            Status::Playing | Status::Paused | Status::Buffering
        )
    }

    fn play_status(&self) -> Status {
        if self.paused {
            Status::Paused
        } else {
            Status::Playing
        }
    }

    /// Waiting for the stream of the current track.
    fn awaiting(&self, video_id: &str) -> bool {
        self.status == Status::Loading
            && !self.load_sent
            && self.queue.current().is_some_and(|t| t.video_id == video_id)
    }

    fn command(&mut self, command: Command, fx: &mut Vec<Effect>) {
        match command {
            Command::PlayTracks { tracks, start } if !tracks.is_empty() => {
                self.queue.replace(tracks, start);
                self.radio_pending = None;
                self.unpause(fx);
                self.change_track(fx);
            }
            Command::PlayNext(tracks) if !tracks.is_empty() && self.queue.cursor().is_some() => {
                self.queue.insert_next(tracks);
                fx.push(Effect::Persist);
                self.sync_next(fx);
            }
            Command::Enqueue(tracks) | Command::PlayNext(tracks) if !tracks.is_empty() => {
                let first_new = self.queue.len();
                self.queue.append(tracks);
                fx.push(Effect::Persist);
                if self.queue.cursor().is_none() && self.queue.jump(first_new) {
                    self.change_track(fx);
                } else {
                    self.sync_next(fx);
                }
            }
            Command::JumpTo(index) => {
                if self.queue.jump(index) {
                    self.unpause(fx);
                    self.change_track(fx);
                }
            }
            Command::RemoveFromQueue(index) => {
                let was_current = self.queue.cursor() == Some(index);
                if !self.queue.remove(index) {
                    return;
                }
                fx.push(Effect::Persist);
                if !was_current {
                    self.sync_next(fx);
                } else if matches!(self.status, Status::Idle | Status::Error) {
                    self.resume_at = None;
                    self.position = 0.0;
                } else {
                    self.resume_at = None;
                    self.start(fx);
                }
            }
            Command::ClearQueue if !self.queue.is_empty() => {
                self.queue.clear();
                self.radio_pending = None;
                self.resume_at = None;
                self.go_idle(fx);
                fx.push(Effect::Persist);
            }
            Command::TogglePause => match self.status {
                _ if self.queue.current().is_none() => {}
                Status::Idle | Status::Error => {
                    self.failures = 0;
                    self.unpause(fx);
                    self.start(fx);
                }
                _ => {
                    self.paused = !self.paused;
                    fx.push(Effect::SetPause(self.paused));
                    if self.loaded() && self.status != Status::Buffering {
                        self.status = self.play_status();
                    }
                }
            },
            Command::Next => {
                if self.queue.advance(true) {
                    self.change_track(fx);
                }
            }
            Command::Prev => {
                if self.loaded() && self.position > 3.0 {
                    self.seek(0.0, fx);
                } else if self.queue.go_back() {
                    self.change_track(fx);
                } else {
                    self.seek(0.0, fx);
                }
            }
            Command::SeekTo(secs) => self.seek(secs, fx),
            Command::SeekBy(delta) => {
                self.seek(self.resume_at.unwrap_or(self.position) + delta, fx)
            }
            Command::SetVolume(volume) => {
                self.volume = volume.min(100);
                fx.push(Effect::SetVolume(self.volume));
                fx.push(Effect::Persist);
            }
            Command::ToggleShuffle => {
                self.queue.toggle_shuffle();
                fx.push(Effect::Persist);
                self.sync_next(fx);
            }
            Command::CycleRepeat => {
                self.queue.set_repeat(match self.queue.repeat() {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                });
                fx.push(Effect::Persist);
                self.sync_next(fx);
            }
            Command::ToggleNormalize => {
                self.normalize = !self.normalize;
                fx.push(Effect::SetNormalize(self.normalize));
                fx.push(Effect::Persist);
            }
            _ => {}
        }
    }

    /// A user-picked track change: fresh failure count, forget the restored position.
    fn change_track(&mut self, fx: &mut Vec<Effect>) {
        self.failures = 0;
        self.retried = false;
        self.resume_at = None;
        fx.push(Effect::Persist);
        self.start(fx);
    }

    /// Start the current track from scratch: stop whatever mpv has, `Loading`, `Resolve`.
    fn start(&mut self, fx: &mut Vec<Effect>) {
        let Some(track) = self.queue.current() else {
            return self.go_idle(fx);
        };
        let id = track.video_id.clone();
        self.duration = track.duration_secs.map_or(0.0, f64::from);
        self.position = 0.0;
        if self.load_sent {
            fx.push(Effect::Stop);
        }
        self.load_sent = false;
        self.status = Status::Loading;
        // The next track's resolve may already be in flight: its answer now serves the current.
        if std::mem::replace(&mut self.next, Next::Empty) != Next::Resolving(id.clone()) {
            fx.push(Effect::Resolve(id));
        }
        self.sync_next(fx);
    }

    fn go_idle(&mut self, fx: &mut Vec<Effect>) {
        if self.load_sent {
            fx.push(Effect::Stop);
        }
        self.status = Status::Idle;
        self.load_sent = false;
        self.next = Next::Empty;
        self.position = 0.0;
        self.duration = 0.0;
    }

    fn unpause(&mut self, fx: &mut Vec<Effect>) {
        if self.paused {
            self.paused = false;
            fx.push(Effect::SetPause(false));
        }
    }

    fn seek(&mut self, secs: f64, fx: &mut Vec<Effect>) {
        if self.status == Status::Loading {
            // Applied by `FileLoaded`; shown meanwhile.
            self.resume_at = Some(secs.max(0.0)); // max() also turns NaN into 0
            return;
        }
        if !self.loaded() {
            return;
        }
        let max = if self.duration > 0.0 {
            self.duration
        } else {
            f64::INFINITY
        };
        self.position = secs.max(0.0).min(max); // max() also turns NaN into 0
        fx.push(Effect::Seek(self.position));
    }

    /// Make mpv's next entry match the queue, and ask for radio when the queue runs low.
    fn sync_next(&mut self, fx: &mut Vec<Effect>) {
        let want = self
            .queue
            .peek_next(false)
            .and_then(|i| self.queue.get(i))
            .map(|t| t.video_id.clone());
        let stale = match &self.next {
            Next::Empty => false,
            Next::Resolving(id) | Next::Appended(id) => want.as_ref() != Some(id),
        };
        if stale {
            if matches!(self.next, Next::Appended(_)) {
                fx.push(Effect::ClearNext);
            }
            self.next = Next::Empty;
        }
        // Also while the current track loads: its answer may already be cached by then.
        let ready = self.loaded() || self.status == Status::Loading;
        if let (true, Next::Empty, Some(id)) = (ready, &self.next, want) {
            fx.push(Effect::Resolve(id.clone()));
            self.next = Next::Resolving(id);
        }
        let active = self.status == Status::Loading || self.loaded();
        if active
            && self.radio_pending.is_none()
            && self.queue.repeat() == Repeat::Off
            && self.queue.remaining() <= 2
        {
            if let Some(track) = self.queue.current() {
                fx.push(Effect::FetchRadio(track.video_id.clone()));
                self.radio_pending = Some(track.video_id.clone());
            }
        }
    }

    /// The current track failed: record it, skip forward, or stop after three in a row.
    fn fail(&mut self, message: String, fx: &mut Vec<Effect>) {
        self.failures = self.failures.saturating_add(1);
        self.retried = false;
        self.last_error = Some(message);
        self.resume_at = None;
        if self.failures >= 3 {
            // Nothing can be appended here: an appended next implies a FileLoaded, which
            // reset the count.
            self.status = Status::Error;
            self.load_sent = false;
            self.next = Next::Empty;
        } else if self.queue.advance(true) {
            fx.push(Effect::Persist);
            self.start(fx);
        } else {
            self.go_idle(fx);
        }
    }
}

/// Apply one event. Returns the effects for the engine, in order.
pub fn reduce(st: &mut State, event: Event) -> Vec<Effect> {
    let mut fx = Vec::new();
    match event {
        Event::Command(command) => st.command(command, &mut fx),
        Event::Resolved { video_id, stream } => {
            if st.awaiting(&video_id) {
                st.load_sent = true;
                fx.push(Effect::Load(stream));
            } else if matches!(&st.next, Next::Resolving(id) if *id == video_id) {
                if st.loaded() {
                    st.next = Next::Appended(video_id);
                    fx.push(Effect::AppendNext(stream));
                } else {
                    // The `Load` of the current track would wipe the append. The engine caches
                    // the stream, so asking again after `FileLoaded` is instant.
                    st.next = Next::Empty;
                }
            }
        }
        Event::ResolveFailed { video_id, message } => {
            if st.awaiting(&video_id) {
                st.fail(message, &mut fx);
            } else if matches!(&st.next, Next::Resolving(id) if *id == video_id) {
                st.next = Next::Empty; // retried when it becomes current
            }
        }
        Event::FileLoaded if st.status == Status::Loading && st.load_sent => {
            st.status = st.play_status();
            st.failures = 0;
            st.retried = false;
            st.last_error = None;
            if let Some(secs) = st.resume_at.take() {
                st.seek(secs, &mut fx);
            }
            st.sync_next(&mut fx);
        }
        // Eof while not loaded is the old file finishing during a track change.
        Event::EndFile(EndReason::Eof) if st.loaded() => {
            let gapless = matches!(st.next, Next::Appended(_));
            // mpv may have moved to an entry cleared a moment ago: the next `start` must `Stop` it.
            st.load_sent = true;
            if !st.queue.advance(false) {
                st.go_idle(&mut fx);
            } else if gapless {
                // mpv is already moving to the appended entry: wait for its FileLoaded.
                st.next = Next::Empty;
                st.status = Status::Loading;
                st.position = 0.0;
                st.duration = st
                    .queue
                    .current()
                    .and_then(|t| t.duration_secs)
                    .map_or(0.0, f64::from);
                fx.push(Effect::Persist);
                st.sync_next(&mut fx);
            } else {
                fx.push(Effect::Persist);
                st.start(&mut fx);
            }
        }
        Event::EndFile(EndReason::Error(message)) if st.load_sent => {
            // With an appended entry mpv moves on by itself, so `start` must `Stop` it.
            st.load_sent = matches!(st.next, Next::Appended(_));
            match st.queue.current().map(|t| t.video_id.clone()) {
                // Refused before it ever played: the stream may be stale or too early.
                Some(id) if st.status == Status::Loading && !st.retried => {
                    st.retried = true;
                    fx.push(Effect::Forget(id));
                    st.start(&mut fx);
                }
                _ => st.fail(message, &mut fx),
            }
        }
        Event::EndFile(EndReason::NoAudio) if st.load_sent => {
            fx.push(Effect::Stop);
            st.load_sent = false;
            st.next = Next::Empty;
            st.status = Status::Error;
            st.last_error = Some("Audio output failed to start".into());
        }
        Event::PauseChanged(paused) => {
            st.paused = paused;
            if st.loaded() && st.status != Status::Buffering {
                st.status = st.play_status();
            }
        }
        Event::TimePos(secs) if st.loaded() => st.position = secs,
        Event::Duration(secs) if st.load_sent => st.duration = secs,
        Event::Buffering(buffering) if st.loaded() => {
            st.status = if buffering {
                Status::Buffering
            } else {
                st.play_status()
            };
        }
        Event::AppendFailed if matches!(st.next, Next::Appended(_)) => st.next = Next::Empty,
        Event::RadioFetched(seed, tracks) if st.radio_pending.as_ref() == Some(&seed) => {
            st.radio_pending = None;
            let mut new: Vec<Track> = Vec::new();
            for t in tracks {
                if !st.queue.contains(&t.video_id) && !new.iter().any(|n| n.video_id == t.video_id)
                {
                    new.push(t);
                }
            }
            if !new.is_empty() {
                st.queue.append(new);
                fx.push(Effect::Persist);
                // The queue ran out while the fetch was pending: carry on.
                if st.status == Status::Idle && st.queue.advance(false) {
                    st.start(&mut fx);
                } else {
                    st.sync_next(&mut fx);
                }
            }
        }
        Event::PlayerRestarted => {
            // The new mpv is empty, unpaused, at default volume.
            fx.push(Effect::SetVolume(st.volume));
            fx.push(Effect::SetNormalize(st.normalize));
            if st.paused {
                fx.push(Effect::SetPause(true));
            }
            if st.loaded() || st.status == Status::Loading {
                st.resume_at = (st.position > 0.0).then_some(st.position);
                st.load_sent = false; // nothing to `Stop` in the new instance
                st.start(&mut fx);
                st.last_error = Some("Player stopped responding, restarting".into());
            }
        }
        Event::Restored {
            queue,
            cursor,
            position_secs,
            volume,
            shuffle,
            repeat,
            normalize,
        } => {
            st.go_idle(&mut fx);
            st.queue.restore(queue, cursor, shuffle, repeat);
            st.position = position_secs.max(0.0);
            st.resume_at = (st.position > 0.0).then_some(st.position);
            st.volume = volume.min(100);
            st.normalize = normalize;
            st.radio_pending = None;
            st.failures = 0;
            st.last_error = None;
            fx.push(Effect::SetVolume(st.volume));
            fx.push(Effect::SetNormalize(normalize));
        }
        _ => {}
    }
    if fx.contains(&Effect::Persist) {
        fx.retain(|e| *e != Effect::Persist);
        fx.push(Effect::Persist);
    }
    fx
}

#[cfg(test)]
mod tests {
    use super::*;
    use Effect::{AppendNext, ClearNext, FetchRadio, Load, Persist, Resolve, Seek, SetPause, Stop};

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            album: None,
            duration_secs: Some(200),
            thumbnail_url: None,
        }
    }

    fn tracks(ids: &str) -> Vec<Track> {
        ids.split_whitespace().map(track).collect()
    }

    fn stream(id: &str) -> Stream {
        Stream {
            url: format!("https://media/{id}"),
            headers: Vec::new(),
        }
    }

    fn resolved(id: &str) -> Event {
        Event::Resolved {
            video_id: id.into(),
            stream: stream(id),
        }
    }

    fn failed(id: &str) -> Event {
        Event::ResolveFailed {
            video_id: id.into(),
            message: format!("{id} failed"),
        }
    }

    fn cmd(command: Command) -> Event {
        Event::Command(command)
    }

    fn play(ids: &str, start: usize) -> Event {
        cmd(Command::PlayTracks {
            tracks: tracks(ids),
            start,
        })
    }

    fn r(id: &str) -> Effect {
        Resolve(id.into())
    }

    fn load(id: &str) -> Effect {
        Load(stream(id))
    }

    fn feed(st: &mut State, events: Vec<Event>) -> Vec<Effect> {
        events.into_iter().flat_map(|e| reduce(st, e)).collect()
    }

    /// Playing track `start` of `ids`; the next track's `Resolve` is already out.
    fn playing(ids: &str, start: usize) -> State {
        let mut st = State::new(7);
        let current = tracks(ids)[start].video_id.clone();
        feed(
            &mut st,
            vec![play(ids, start), resolved(&current), Event::FileLoaded],
        );
        assert_eq!(st.status, Status::Playing);
        st
    }

    fn ids(st: &State) -> String {
        let q = st.snapshot().queue;
        q.iter()
            .map(|t| t.video_id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    const ABCDE: &str = "a b c d e";

    #[test]
    fn flows() {
        use Event::{AppendFailed, EndFile, FileLoaded};
        let radio = |seed: &str, ids| Event::RadioFetched(seed.into(), tracks(ids));
        use Status::{Buffering, Error, Idle, Loading, Paused, Playing};
        let eof = || EndFile(EndReason::Eof);
        #[rustfmt::skip]
        let cases = vec![
            ("play starts loading", State::new(1), vec![play(ABCDE, 0)],
                vec![r("a"), r("b"), Persist], Loading, Some(0)),
            ("loaded track resolves the next one", State::new(1),
                vec![play(ABCDE, 0), resolved("a"), FileLoaded],
                vec![r("a"), r("b"), Persist, load("a")], Playing, Some(0)),
            ("eof advances, reusing the in-flight resolve", playing(ABCDE, 0),
                vec![eof(), resolved("b"), FileLoaded],
                vec![Stop, r("c"), Persist, load("b")], Playing, Some(1)),
            ("gapless advance with appended next", playing(ABCDE, 0),
                vec![resolved("b"), eof(), FileLoaded],
                vec![AppendNext(stream("b")), r("c"), Persist], Playing, Some(1)),
            ("eof racing a cleared next stops mpv's own advance", playing(ABCDE, 0),
                vec![resolved("b"), cmd(Command::PlayNext(tracks("x"))), eof()],
                vec![AppendNext(stream("b")), ClearNext, r("x"), Persist, Stop, r("b"), Persist],
                Loading, Some(1)),
            ("failed append falls back to a normal advance", playing(ABCDE, 0),
                vec![resolved("b"), AppendFailed, eof()],
                vec![AppendNext(stream("b")), Stop, r("b"), r("c"), Persist], Loading, Some(1)),
            ("last tracks trigger autoplay fetch", State::new(1), vec![play("a b", 0)],
                vec![r("a"), r("b"), FetchRadio("a".into()), Persist], Loading, Some(0)),
            ("radio continues the queue", playing("a", 0),
                vec![radio("a", "a x y z"), resolved("x"), eof(), FileLoaded],
                vec![r("x"), Persist, AppendNext(stream("x")), r("y"), FetchRadio("x".into()),
                     Persist], Playing, Some(1)),
            ("radio arriving after the end resumes", playing("a", 0),
                vec![eof(), radio("a", "x")],
                vec![Stop, r("x"), FetchRadio("x".into()), Persist], Loading, Some(1)),
            ("queue end without radio goes idle", playing("a", 0),
                vec![radio("a", ""), eof()], vec![Stop], Idle, Some(0)),
            ("failed resolve skips", State::new(1), vec![play(ABCDE, 0), failed("a")],
                vec![r("a"), r("b"), Persist, r("c"), Persist], Loading, Some(1)),
            ("load error retries once with a fresh resolve", State::new(1),
                vec![play(ABCDE, 0), resolved("a"), EndFile(EndReason::Error("403".into()))],
                vec![r("a"), r("b"), Persist, load("a"), Effect::Forget("a".into()), r("a"),
                     r("b")], Loading, Some(0)),
            ("load error twice skips", State::new(1),
                vec![play(ABCDE, 0), resolved("a"), EndFile(EndReason::Error("403".into())),
                     resolved("a"), EndFile(EndReason::Error("403".into()))],
                vec![r("a"), r("b"), Persist, load("a"), Effect::Forget("a".into()), r("a"),
                     r("b"), load("a"), r("c"), Persist], Loading, Some(1)),
            ("playback error with appended next stops mpv's own advance", playing(ABCDE, 0),
                vec![resolved("b"), EndFile(EndReason::Error("net".into()))],
                vec![AppendNext(stream("b")), Stop, r("b"), r("c"), Persist], Loading, Some(1)),
            ("three failures stop in Error", State::new(1),
                vec![play(ABCDE, 0), failed("a"), failed("b"), failed("c"), resolved("d"),
                     resolved("c"), FileLoaded],
                vec![r("a"), r("b"), Persist, r("c"), Persist, r("d"), FetchRadio("c".into()), Persist],
                Error, Some(2)),
            ("FileLoaded resets the failure count", State::new(1),
                vec![play(ABCDE, 0), failed("a"), failed("b"), resolved("c"), FileLoaded,
                     EndFile(EndReason::Error("x".into())), failed("d")],
                vec![r("a"), r("b"), Persist, r("c"), Persist, r("d"), FetchRadio("c".into()), Persist,
                     load("c"), r("e"), Persist, Persist], Loading, Some(4)),
            ("stopped end-file is ignored", playing(ABCDE, 0),
                vec![EndFile(EndReason::Stopped)], vec![], Playing, Some(0)),
            ("pause and resume", playing(ABCDE, 0),
                vec![cmd(Command::TogglePause), Event::PauseChanged(true),
                     cmd(Command::TogglePause)],
                vec![SetPause(true), SetPause(false)], Playing, Some(0)),
            ("mpv pause wins", playing(ABCDE, 0), vec![Event::PauseChanged(true)],
                vec![], Paused, Some(0)),
            ("buffering", playing(ABCDE, 0), vec![Event::Buffering(true)],
                vec![], Buffering, Some(0)),
            ("repeat one replays gaplessly", playing(ABCDE, 0),
                vec![cmd(Command::CycleRepeat), cmd(Command::CycleRepeat), resolved("a"), eof(),
                     FileLoaded],
                vec![Persist, r("a"), Persist, AppendNext(stream("a")), r("a"), Persist],
                Playing, Some(0)),
            ("repeat all wraps", State::new(1),
                vec![cmd(Command::CycleRepeat), play(ABCDE, 4), resolved("e"), FileLoaded, eof()],
                vec![Persist, r("e"), r("a"), Persist, load("e"), Stop, r("b"), Persist], Loading, Some(0)),
            ("play-next replaces the appended next", playing(ABCDE, 0),
                vec![resolved("b"), cmd(Command::PlayNext(tracks("x")))],
                vec![AppendNext(stream("b")), ClearNext, r("x"), Persist], Playing, Some(0)),
            ("enqueue at the end resolves the new next", playing("a", 0),
                vec![cmd(Command::Enqueue(tracks("b")))], vec![r("b"), Persist], Playing, Some(0)),
            ("remove after the cursor changes next", playing(ABCDE, 0),
                vec![resolved("b"), cmd(Command::RemoveFromQueue(1))],
                vec![AppendNext(stream("b")), ClearNext, r("c"), Persist], Playing, Some(0)),
            ("remove before the cursor keeps next", playing(ABCDE, 2),
                vec![resolved("d"), cmd(Command::RemoveFromQueue(0))],
                vec![AppendNext(stream("d")), Persist], Playing, Some(1)),
            ("remove the current track starts the following one", playing(ABCDE, 1),
                // "c" was already resolving as the next track, so no new Resolve.
                vec![cmd(Command::RemoveFromQueue(1))], vec![Stop, r("d"), FetchRadio("c".into()), Persist],
                Loading, Some(1)),
            ("remove the last current track goes idle", playing("a", 0),
                vec![cmd(Command::RemoveFromQueue(0))], vec![Stop, Persist], Idle, None),
            ("prev late in a track restarts it", playing(ABCDE, 2),
                vec![Event::TimePos(10.0), cmd(Command::Prev)], vec![Seek(0.0)], Playing, Some(2)),
            ("prev early goes back", playing(ABCDE, 2),
                vec![Event::TimePos(1.0), cmd(Command::Prev)],
                vec![Stop, r("b"), r("c"), Persist], Loading, Some(1)),
            ("prev on the first track restarts it", playing(ABCDE, 0),
                vec![cmd(Command::Prev)], vec![Seek(0.0)], Playing, Some(0)),
            ("clear stops", playing(ABCDE, 0), vec![cmd(Command::ClearQueue)],
                vec![Stop, Persist], Idle, None),
        ];
        for (name, mut st, events, want, status, cursor) in cases {
            let got = feed(&mut st, events);
            assert_eq!(got, want, "{name}");
            assert_eq!((st.status, st.queue.cursor()), (status, cursor), "{name}");
        }
    }

    #[test]
    fn next_answer_before_the_current_loads_is_not_appended() {
        let mut st = State::new(1);
        let fx = feed(&mut st, vec![play("a b c d e", 0), resolved("b")]);
        assert_eq!(
            fx,
            [r("a"), r("b"), Persist],
            "no AppendNext yet: Load would wipe it"
        );
        let fx = feed(&mut st, vec![resolved("a"), Event::FileLoaded]);
        // Asked again once loaded (the engine answers from its cache).
        assert_eq!(fx, [load("a"), r("b")]);
        assert_eq!(
            feed(&mut st, vec![resolved("b")]),
            [AppendNext(stream("b"))]
        );
        // A manual Next re-asks for the track; the engine cache makes it instant.
        let fx = feed(&mut st, vec![cmd(Command::Next)]);
        assert_eq!(fx[..2], [Stop, r("b")]);
    }

    #[test]
    fn seek_while_loading_is_applied_when_loaded() {
        let mut st = State::new(1);
        feed(&mut st, vec![play("a b", 0), cmd(Command::SeekTo(30.0))]);
        assert_eq!(st.snapshot().position_secs, 30.0, "shown while loading");
        feed(&mut st, vec![cmd(Command::SeekBy(5.0))]);
        feed(&mut st, vec![resolved("a")]);
        let fx = feed(&mut st, vec![Event::FileLoaded]);
        assert!(fx.contains(&Seek(35.0)), "{fx:?}");
        assert_eq!(st.snapshot().position_secs, 35.0);
        // A pick of another track drops it.
        let mut st = State::new(1);
        feed(
            &mut st,
            vec![
                play("a b", 0),
                cmd(Command::SeekTo(30.0)),
                cmd(Command::Next),
            ],
        );
        let fx = feed(&mut st, vec![resolved("b"), Event::FileLoaded]);
        assert!(!fx.iter().any(|e| matches!(e, Seek(_))), "{fx:?}");
    }

    #[test]
    fn shuffle_toggle_resyncs_next() {
        let mut st = playing(ABCDE, 2);
        feed(&mut st, vec![resolved("d")]);
        let fx = feed(&mut st, vec![cmd(Command::ToggleShuffle)]);
        let next = st.queue.get(1).unwrap().video_id.clone();
        assert_eq!(st.queue.current().unwrap().video_id, "c");
        assert_eq!(fx.last(), Some(&Persist));
        if next != "d" {
            assert_eq!(fx[..2], [ClearNext, r(&next)]);
        }
        feed(&mut st, vec![cmd(Command::ToggleShuffle)]);
        assert_eq!(ids(&st), ABCDE);
        assert_eq!(st.queue.cursor(), Some(2));
        assert!(matches!(&st.next, Next::Resolving(id) | Next::Appended(id) if id == "d"));
    }

    #[test]
    fn fast_skipping_plays_exactly_the_last_pick() {
        let mut st = playing("a b c d e f g h", 0);
        let mut events = vec![cmd(Command::Next); 5];
        // Answers for every skipped track arrive late, in a scrambled order.
        events.extend(["c", "b", "f", "e", "d", "g"].map(resolved));
        events.push(Event::FileLoaded);
        let fx = feed(&mut st, events);
        let loads: Vec<_> = fx.iter().filter(|e| matches!(e, Load(_))).collect();
        assert_eq!(loads, [&load("f")]);
        assert_eq!(fx.iter().filter(|e| **e == Stop).count(), 1);
        assert_eq!(st.status, Status::Playing);
        assert_eq!(st.snapshot().current().unwrap().video_id, "f");
        // "g" was answered before f loaded, so it is asked again now.
        assert_eq!(
            fx.iter().rev().find(|e| matches!(e, Resolve(_))),
            Some(&r("g"))
        );
    }

    #[test]
    fn errors_are_recorded_and_cleared() {
        let mut st = State::new(1);
        feed(&mut st, vec![play(ABCDE, 0), failed("a")]);
        assert_eq!(st.snapshot().last_error.as_deref(), Some("a failed"));
        feed(&mut st, vec![resolved("b"), Event::FileLoaded]);
        assert_eq!(st.snapshot().last_error, None);
        // Error state: a user command starts over with a fresh count.
        let mut st = State::new(1);
        feed(
            &mut st,
            vec![play(ABCDE, 0), failed("a"), failed("b"), failed("c")],
        );
        assert_eq!(
            feed(&mut st, vec![cmd(Command::Next)]),
            [r("d"), r("e"), Persist]
        );
        assert_eq!(feed(&mut st, vec![failed("d")]), [Persist]);
    }

    #[test]
    fn radio_is_fetched_once_and_skips_duplicates() {
        let mut st = playing("a b", 0);
        let fx = feed(&mut st, vec![resolved("b"), Event::EndFile(EndReason::Eof)]);
        assert!(
            !fx.iter().any(|e| matches!(e, FetchRadio(_))),
            "still pending: {fx:?}"
        );
        let radio = |seed: &str, ids| Event::RadioFetched(seed.into(), tracks(ids));
        feed(&mut st, vec![radio("a", "a b x x y")]);
        assert_eq!(ids(&st), "a b x y");
        // A late answer after the queue was replaced is dropped.
        let mut st = playing(ABCDE, 3);
        feed(&mut st, vec![play("p q r s t u", 0)]);
        feed(&mut st, vec![radio("d", "z")]);
        assert_eq!(ids(&st), "p q r s t u");
        // Even when the new queue has its own fetch pending: only its answer counts.
        let mut st = playing("a b", 0);
        let fx = feed(&mut st, vec![play("p q", 0)]);
        assert!(fx.contains(&FetchRadio("p".into())), "{fx:?}");
        feed(&mut st, vec![radio("a", "x"), radio("p", "y")]);
        assert_eq!(ids(&st), "p q y");
    }

    #[test]
    fn restore_comes_back_idle_and_resumes() {
        let mut st = State::new(1);
        let fx = feed(
            &mut st,
            vec![Event::Restored {
                queue: tracks(ABCDE),
                cursor: Some(1),
                position_secs: 42.0,
                volume: 150,
                shuffle: false,
                repeat: Repeat::All,
                normalize: true,
            }],
        );
        assert_eq!(fx, [Effect::SetVolume(100), Effect::SetNormalize(true)]);
        let snap = st.snapshot();
        assert_eq!(
            (snap.status, snap.cursor, snap.position_secs),
            (Status::Idle, Some(1), 42.0)
        );
        let fx = feed(
            &mut st,
            vec![cmd(Command::TogglePause), resolved("b"), Event::FileLoaded],
        );
        assert_eq!(fx, [r("b"), r("c"), load("b"), Seek(42.0)]);
    }

    #[test]
    fn no_audio_device_is_an_error_not_a_skip() {
        let mut st = playing("a b c", 0);
        let fx = reduce(&mut st, Event::EndFile(EndReason::NoAudio));
        let snap = st.snapshot();
        assert_eq!((snap.status, snap.cursor), (Status::Error, Some(0)));
        assert_eq!(
            snap.last_error.as_deref(),
            Some("Audio output failed to start")
        );
        assert_eq!(fx, [Stop]);
    }

    #[test]
    fn restart_reloads_the_current_track_at_its_position() {
        let mut st = playing("a b c", 0);
        feed(&mut st, vec![Event::TimePos(42.0)]);
        let fx = reduce(&mut st, Event::PlayerRestarted);
        assert!(!fx.contains(&Stop) && fx.contains(&r("a")), "{fx:?}");
        assert_eq!(st.snapshot().position_secs, 42.0);
        let fx = feed(&mut st, vec![resolved("a"), Event::FileLoaded]);
        assert!(fx.contains(&Seek(42.0)), "{fx:?}");
    }

    #[test]
    fn restored_position_survives_resolving_but_not_a_failure() {
        let mut st = State::new(1);
        feed(
            &mut st,
            vec![Event::Restored {
                queue: tracks(ABCDE),
                cursor: Some(1),
                position_secs: 42.0,
                volume: 100,
                shuffle: false,
                repeat: Repeat::Off,
                normalize: false,
            }],
        );
        feed(&mut st, vec![cmd(Command::TogglePause)]);
        assert_eq!(st.status, Status::Loading);
        assert_eq!(st.snapshot().position_secs, 42.0, "saved while resolving");
        let fx = feed(&mut st, vec![failed("b"), resolved("c"), Event::FileLoaded]);
        assert!(!fx.iter().any(|e| matches!(e, Seek(_))), "{fx:?}");
        assert_eq!(st.snapshot().position_secs, 0.0);
    }

    #[test]
    fn volume_and_seek_clamp() {
        let mut st = playing(ABCDE, 0);
        let fx = feed(
            &mut st,
            vec![
                cmd(Command::SetVolume(255)),
                Event::Duration(100.0),
                Event::TimePos(90.0),
                cmd(Command::SeekBy(30.0)),
                cmd(Command::SeekBy(-500.0)),
                cmd(Command::SeekTo(f64::NAN)),
            ],
        );
        assert_eq!(
            fx,
            [
                Effect::SetVolume(100),
                Persist,
                Seek(100.0),
                Seek(0.0),
                Seek(0.0)
            ]
        );
    }

    #[test]
    fn snapshot_reflects_state() {
        let mut st = playing(ABCDE, 1);
        feed(
            &mut st,
            vec![
                Event::TimePos(12.5),
                Event::Duration(180.0),
                cmd(Command::SetVolume(30)),
                cmd(Command::CycleRepeat),
                cmd(Command::ToggleNormalize),
            ],
        );
        let snap = st.snapshot();
        assert_eq!(snap.status, Status::Playing);
        assert_eq!(snap.current().map(|t| t.video_id.as_str()), Some("b"));
        assert_eq!(snap.queue.len(), 5);
        assert_eq!((snap.position_secs, snap.duration_secs), (12.5, 180.0));
        assert_eq!(
            (snap.volume, snap.repeat, snap.normalize),
            (30, Repeat::All, true)
        );
        assert!(!snap.shuffle);
        // TimePos alone never asks for a save.
        assert_eq!(feed(&mut st, vec![Event::TimePos(13.0)]), []);
    }

    #[test]
    fn empty_queue_commands_are_noops() {
        let commands = vec![
            Command::Enqueue(Vec::new()),
            Command::PlayNext(Vec::new()),
            Command::PlayTracks {
                tracks: Vec::new(),
                start: 3,
            },
            Command::JumpTo(0),
            Command::RemoveFromQueue(0),
            Command::ClearQueue,
            Command::TogglePause,
            Command::Next,
            Command::Prev,
            Command::SeekTo(10.0),
            Command::SeekBy(-10.0),
            Command::Quit,
        ];
        for c in commands {
            let mut st = State::new(1);
            assert_eq!(reduce(&mut st, cmd(c.clone())), [], "{c:?}");
            assert_eq!(st.snapshot(), State::new(1).snapshot(), "{c:?}");
        }
    }

    #[test]
    fn any_event_order_never_panics() {
        let pool = || {
            vec![
                play("a b c", 1),
                cmd(Command::Enqueue(tracks("d"))),
                cmd(Command::PlayNext(tracks("e"))),
                cmd(Command::JumpTo(2)),
                cmd(Command::RemoveFromQueue(0)),
                cmd(Command::RemoveFromQueue(1)),
                cmd(Command::ClearQueue),
                cmd(Command::TogglePause),
                cmd(Command::Next),
                cmd(Command::Prev),
                cmd(Command::SeekBy(5.0)),
                cmd(Command::ToggleShuffle),
                cmd(Command::CycleRepeat),
                resolved("a"),
                resolved("b"),
                resolved("c"),
                resolved("d"),
                failed("b"),
                failed("c"),
                Event::FileLoaded,
                Event::EndFile(EndReason::Eof),
                Event::EndFile(EndReason::Error("x".into())),
                Event::EndFile(EndReason::Stopped),
                Event::PauseChanged(true),
                Event::PauseChanged(false),
                Event::TimePos(4.0),
                Event::Duration(f64::NAN),
                Event::Buffering(true),
                Event::Buffering(false),
                Event::RadioFetched("a".into(), tracks("a r s")),
                Event::RadioFetched("b".into(), Vec::new()),
                Event::AppendFailed,
            ]
        };
        let n = pool().len() as u64;
        for seed in 1..300u64 {
            let mut st = State::new(seed);
            let mut x = seed;
            for _ in 0..60 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let event = pool().swap_remove((x % n) as usize);
                reduce(&mut st, event);
                let snap = st.snapshot();
                assert!(snap.cursor.is_none_or(|c| c < snap.queue.len()));
                assert!(snap.volume <= 100);
            }
        }
    }
}
