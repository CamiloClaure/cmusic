//! State, messages and the pure update logic. `State::update` never touches the engine or the
//! network: it returns an `Out` describing what the thin wrapper in `main.rs` must do.

use crate::theme::{self, Look};
use cmusic_core::{
    lyrics, Command, CookieSource, Item, Lyrics, Page, PageRequest, Rating, Snapshot, Track, Update,
};
use iced::widget::image;
use iced::Size;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Widget ids the wrapper needs to aim a focus or scroll operation at.
pub const SEARCH: &str = "search";
pub const LYRICS: &str = "lyrics";
pub const LIST: &str = "list";
/// Window width below which the queue is a toggled view instead of a column.
pub const NARROW: f32 = 980.0;
/// Height of one lyrics line at font scale 1; fixed so the scroll offset is exact.
pub const LYRIC_LH: f32 = 34.0;
/// A manual scroll in the lyrics view pauses auto-scroll for this long.
const USER_SCROLL_HOLD: Duration = Duration::from_secs(5);

/// A tiny, heavily blurred copy of the art; the renderer stretches it over the window.
// ponytail: runs inside update (a few ms on a 64 px image); move to a Task if it ever shows.
fn blurred(bytes: &[u8], look: &Look) -> Option<image::Handle> {
    const N: u32 = 64;
    let img = ::image::load_from_memory(bytes).ok()?;
    let mut px = img
        .resize_to_fill(N, N, ::image::imageops::FilterType::Triangle)
        .blur(look.backdrop_blur)
        .to_rgba8();
    // Covers are often dull once blurred: push saturation so the glass has colour to refract.
    for p in px.pixels_mut() {
        let [r, g, b, _] = p.0.map(f32::from);
        let grey = 0.3 * r + 0.59 * g + 0.11 * b;
        let lift = |c: f32| {
            ((grey + (c - grey) * look.backdrop_saturation) * 1.15).clamp(0.0, 255.0) as u8
        };
        p.0 = [lift(r), lift(g), lift(b), 255];
    }
    Some(image::Handle::from_rgba(N, N, px.into_raw()))
}

/// Video-type thumbnails have black bars baked in (a 16:9 frame letterboxed into a square).
/// Crops them off to the centred square of the picture (re-encoded as PNG); anything else comes
/// back untouched. Called from the fetch task, never from the view.
// ponytail: a cover that really has near-black bands top and bottom gets cropped too.
pub fn trim_bars(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(img) = ::image::load_from_memory(&bytes) else {
        return bytes;
    };
    let (w, h) = (img.width(), img.height());
    let px = img.to_rgb8();
    let dark = |y: u32| (0..w).all(|x| px.get_pixel(x, y).0.iter().all(|&c| c < 40));
    let top = (0..h).take_while(|&y| dark(y)).count() as u32;
    let bottom = (0..h).rev().take_while(|&y| dark(y)).count() as u32;
    if top < h / 16 || bottom < h / 16 || top + bottom >= h {
        return bytes;
    }
    let side = (h - top - bottom).min(w);
    // Real letterboxing leaves a wide picture; anything else is just a dark cover.
    if w * 10 < side * 12 {
        return bytes;
    }
    let mut out = std::io::Cursor::new(Vec::new());
    match img
        .crop_imm(
            (w - side) / 2,
            top + (h - top - bottom - side) / 2,
            side,
            side,
        )
        .write_to(&mut out, ::image::ImageFormat::Png)
    {
        Ok(()) => out.into_inner(),
        Err(_) => bytes,
    }
}

/// Art is requested for this many rows when a page loads.
// ponytail: later rows keep the placeholder; request on scroll if that bothers anyone.
pub const ART_ROWS: usize = 30;
const HISTORY_MAX: usize = 20;

#[derive(Debug, Clone)]
pub enum Message {
    Engine(Update),
    EngineStopped,
    SearchInput(String),
    SearchSubmit,
    Open(PageRequest),
    Back,
    PlayRow(usize),
    PlayNext(Track),
    Enqueue(Track),
    Like(String),
    TogglePause,
    Next,
    Prev,
    SeekChanged(f64),
    SeekReleased,
    SeekBy(f64),
    VolumeBy(i8),
    Volume(u8),
    Shuffle,
    Repeat,
    Normalize,
    ToggleLyrics,
    ToggleQueue,
    FocusSearch,
    Escape,
    /// Viewport height of the lyrics scrollable, sent whenever its offset changes.
    LyricsScrolled(f32),
    /// Clears the notice if it is still the one with this generation.
    ClearNotice(u64),
    Jump(usize),
    Remove(usize),
    ClearQueue,
    OpenLogin,
    CloseLogin,
    ImportBrowsers,
    CookieInput(String),
    UseCookie,
    Art(String, Option<Vec<u8>>),
    Resized(Size),
    CloseRequested,
    CycleTheme,
    ReloadTheme,
}

#[derive(Debug, Default)]
pub struct Login {
    pub message: Option<String>,
    pub cookie: String,
}

/// What the wrapper must do after an update.
#[derive(Debug, Default)]
pub struct Out {
    pub commands: Vec<Command>,
    pub art: Vec<String>,
    /// Quit was sent: save the window size and exit once the engine stream ends.
    pub closing: bool,
    /// The engine stream ended: exit now.
    pub stopped: bool,
    pub focus_search: bool,
    pub scroll: Option<Scroll>,
    /// A notice was set: send `ClearNotice` with this generation after a delay.
    pub notice_gen: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scroll {
    ListTop,
    /// Absolute offset in px for the lyrics scrollable.
    Lyrics(f32),
}

pub struct State {
    pub snapshot: Snapshot,
    pub search: String,
    /// Request in flight, if any.
    pub awaiting: Option<PageRequest>,
    pub current: Option<(PageRequest, Page)>,
    pub history: Vec<(PageRequest, Page)>,
    pub lyrics: Option<(String, Option<Lyrics>)>,
    pub show_lyrics: bool,
    /// Narrow windows only: the queue replaces the main area.
    pub queue_open: bool,
    /// Set by the wrapper before each update, so `update` stays deterministic.
    pub now: Instant,
    notice_gen: u64,
    lyric_line: Option<usize>,
    user_scrolled: Option<Instant>,
    /// We issued a scroll and its `on_scroll` echo has not arrived yet.
    scroll_pending: bool,
    lyrics_h: f32,
    pub login: Option<Login>,
    pub notice: Option<String>,
    /// Set at startup from the cookie file, then by the "Logged in" notice and `AuthRequired`.
    pub logged_in: bool,
    /// Local slider value while the seek slider is being dragged.
    pub drag: Option<f64>,
    pub art: HashMap<String, image::Handle>,
    /// Blurred art of the current track (keyed by its url), drawn behind everything.
    pub backdrop: Option<(String, image::Handle)>,
    /// Video ids liked in this session or listed on the liked-songs page.
    // ponytail: session-local; the engine has no "is liked" query, so a like made elsewhere or
    // outside the open liked page stays unfilled until that page is opened.
    pub liked: HashSet<String>,
    requested: HashSet<String>,
    pub size: Size,
    pub look: Look,
    /// `theme::DEFAULT` or the stem of a file in the themes dir.
    pub theme_name: String,
}

impl State {
    pub fn new(size: Size) -> Self {
        Self {
            snapshot: Snapshot::default(),
            search: String::new(),
            awaiting: None,
            current: None,
            history: Vec::new(),
            lyrics: None,
            show_lyrics: false,
            queue_open: false,
            now: Instant::now(),
            notice_gen: 0,
            lyric_line: None,
            user_scrolled: None,
            scroll_pending: false,
            lyrics_h: 0.0,
            login: None,
            notice: None,
            logged_in: false,
            drag: None,
            art: HashMap::new(),
            backdrop: None,
            liked: HashSet::new(),
            requested: HashSet::new(),
            size,
            look: Look::default(),
            theme_name: theme::DEFAULT.into(),
        }
    }

    pub fn update(&mut self, msg: Message) -> Out {
        let gen = self.notice_gen;
        let mut out = self.step(msg);
        if self.notice_gen != gen {
            out.notice_gen = Some(self.notice_gen);
        }
        out
    }

    fn set_notice(&mut self, n: String) {
        self.notice = Some(n);
        self.notice_gen += 1;
    }

    fn step(&mut self, msg: Message) -> Out {
        let mut out = Out::default();
        match msg {
            Message::Engine(u) => return self.engine(u),
            Message::EngineStopped => out.stopped = true,
            Message::SearchInput(s) => self.search = s,
            Message::SearchSubmit => {
                let q = self.search.trim().to_string();
                if !q.is_empty() {
                    self.fetch(PageRequest::Search(q), &mut out);
                }
            }
            Message::Open(r) => self.fetch(r, &mut out),
            Message::Back => self.back(&mut out),
            Message::Escape => {
                if self.login.is_some() {
                    self.login = None;
                } else if self.show_lyrics {
                    self.show_lyrics = false;
                } else if self.queue_open {
                    self.queue_open = false;
                } else {
                    self.back(&mut out);
                }
            }
            Message::FocusSearch => out.focus_search = true,
            Message::ToggleQueue => self.queue_open = !self.queue_open,
            Message::LyricsScrolled(h) => {
                self.lyrics_h = h;
                if self.scroll_pending {
                    self.scroll_pending = false;
                } else {
                    self.user_scrolled = Some(self.now);
                }
            }
            Message::ClearNotice(g) => {
                if g == self.notice_gen {
                    self.notice = None;
                }
            }
            Message::SeekBy(d) => out.commands.push(Command::SeekBy(d)),
            Message::VolumeBy(d) => {
                let v = (i16::from(self.snapshot.volume) + i16::from(d)).clamp(0, 100) as u8;
                out.commands.push(Command::SetVolume(v));
            }
            Message::PlayRow(i) => {
                if let Some((_, page)) = &self.current {
                    let tracks: Vec<Track> = page
                        .items
                        .iter()
                        .filter_map(|it| match it {
                            Item::Track(t) => Some(t.clone()),
                            _ => None,
                        })
                        .collect();
                    let start = page.items[..i.min(page.items.len())]
                        .iter()
                        .filter(|it| matches!(it, Item::Track(_)))
                        .count();
                    match page.items.get(i) {
                        // The playing track: pause/resume instead of rebuilding the queue.
                        Some(Item::Track(t))
                            if self
                                .snapshot
                                .current()
                                .is_some_and(|c| c.video_id == t.video_id) =>
                        {
                            out.commands.push(Command::TogglePause);
                        }
                        Some(Item::Track(_)) => {
                            out.commands.push(Command::PlayTracks { tracks, start });
                        }
                        _ => {}
                    }
                }
            }
            Message::PlayNext(t) => out.commands.push(Command::PlayNext(vec![t])),
            Message::Enqueue(t) => out.commands.push(Command::Enqueue(vec![t])),
            Message::Like(video_id) => {
                let rating = if self.liked.remove(&video_id) {
                    Rating::None
                } else {
                    self.liked.insert(video_id.clone());
                    Rating::Like
                };
                out.commands.push(Command::Rate { video_id, rating });
            }
            Message::TogglePause => out.commands.push(Command::TogglePause),
            Message::Next => out.commands.push(Command::Next),
            Message::Prev => out.commands.push(Command::Prev),
            Message::SeekChanged(v) => self.drag = Some(v),
            Message::SeekReleased => {
                if let Some(v) = self.drag.take() {
                    out.commands.push(Command::SeekTo(v));
                }
            }
            Message::Volume(v) => out.commands.push(Command::SetVolume(v)),
            Message::Shuffle => out.commands.push(Command::ToggleShuffle),
            Message::Repeat => out.commands.push(Command::CycleRepeat),
            Message::Normalize => out.commands.push(Command::ToggleNormalize),
            Message::ToggleLyrics => {
                self.show_lyrics = !self.show_lyrics;
                self.lyric_line = None;
                self.lyric_scroll(&mut out);
            }
            Message::Jump(i) => out.commands.push(Command::JumpTo(i)),
            Message::Remove(i) => out.commands.push(Command::RemoveFromQueue(i)),
            Message::ClearQueue => out.commands.push(Command::ClearQueue),
            Message::OpenLogin => self.login = Some(Login::default()),
            Message::CloseLogin => self.login = None,
            Message::ImportBrowsers => out
                .commands
                .push(Command::ImportCookies(CookieSource::Browsers)),
            Message::CookieInput(s) => {
                if let Some(l) = &mut self.login {
                    l.cookie = s;
                }
            }
            Message::UseCookie => {
                if let Some(l) = &self.login {
                    let c = l.cookie.trim().to_string();
                    if !c.is_empty() {
                        out.commands
                            .push(Command::ImportCookies(CookieSource::Header(c)));
                    }
                }
            }
            Message::Art(url, bytes) => {
                if let Some(b) = bytes {
                    self.art.insert(url, image::Handle::from_bytes(b));
                    self.refresh_backdrop();
                }
            }
            Message::Resized(s) => self.size = s,
            Message::CloseRequested => {
                out.commands.push(Command::Quit);
                out.closing = true;
            }
            Message::CycleTheme => {
                let mut all = vec![theme::DEFAULT.to_string()];
                all.extend(theme::list());
                let at = all.iter().position(|n| *n == self.theme_name).unwrap_or(0);
                let next = all[(at + 1) % all.len()].clone();
                self.set_theme(next);
            }
            Message::ReloadTheme => self.set_theme(self.theme_name.clone()),
        }
        out
    }

    /// On failure the current look stays and the error becomes the notice.
    // ponytail: reads the file inside update (a few hundred bytes); move to a Task if it ever shows.
    fn set_theme(&mut self, name: String) {
        let loaded = theme::load(&name);
        // The name sticks even when broken, so cycling moves on and Ctrl+R retries the file.
        self.theme_name = name;
        match loaded {
            Ok(look) => {
                self.look = look;
                self.set_notice(format!("Theme: {}", self.theme_name));
                self.backdrop = None;
                self.refresh_backdrop();
            }
            Err(e) => self.set_notice(e),
        }
    }

    fn back(&mut self, out: &mut Out) {
        if let Some(prev) = self.history.pop() {
            self.current = Some(prev);
            self.awaiting = None;
            self.show_lyrics = false;
            // The list widget keeps its scroll offset across pages.
            out.scroll = Some(Scroll::ListTop);
        }
    }

    /// Asks for the lyrics view to centre the current line when it just changed, unless the
    /// user scrolled by hand a moment ago.
    fn lyric_scroll(&mut self, out: &mut Out) {
        if !self.show_lyrics || self.login.is_some() {
            return;
        }
        let Some(i) = self
            .current_lyrics()
            .and_then(|ly| lyrics::current_line(ly, self.snapshot.position_secs))
        else {
            return;
        };
        if self.lyric_line == Some(i) {
            return;
        }
        self.lyric_line = Some(i);
        if self
            .user_scrolled
            .is_some_and(|t| self.now.duration_since(t) < USER_SCROLL_HOLD)
        {
            return;
        }
        let l = self.look;
        // Window minus outer and panel padding, the bar, the header and the content padding.
        let est = self.size.height - 7.0 * l.gap - 64.0 - 36.0 - l.pad;
        let h = if self.lyrics_h > 0.0 {
            self.lyrics_h
        } else {
            est
        };
        let lh = LYRIC_LH * l.font_scale;
        // ponytail: if the on_scroll echo never comes (offset clamped at the end) the next manual
        // scroll is not recognised as manual; harmless.
        self.scroll_pending = true;
        out.scroll = Some(Scroll::Lyrics(
            ((i as f32 + 0.5) * lh + l.pad - h / 2.0).max(0.0),
        ));
    }

    fn fetch(&mut self, r: PageRequest, out: &mut Out) {
        self.awaiting = Some(r.clone());
        out.commands.push(Command::Fetch(r));
    }

    fn engine(&mut self, u: Update) -> Out {
        let mut out = Out::default();
        match u {
            Update::Player(s) => {
                let before = self.snapshot.current().map(|t| t.video_id.clone());
                let url = s.current().and_then(|t| t.thumbnail_url.clone());
                if before.as_deref() != s.current().map(|t| t.video_id.as_str()) {
                    out.art = self.want_art([url]);
                }
                self.snapshot = s;
                self.refresh_backdrop();
                self.lyric_scroll(&mut out);
            }
            Update::Page { request, result } => {
                if self.awaiting.as_ref() != Some(&request) {
                    return out;
                }
                self.awaiting = None;
                match result {
                    Ok(page) => {
                        if request == PageRequest::LikedSongs {
                            self.liked
                                .extend(page.items.iter().filter_map(|it| match it {
                                    Item::Track(t) => Some(t.video_id.clone()),
                                    _ => None,
                                }));
                        }
                        out.art = self.want_art(page.items.iter().take(ART_ROWS).map(thumb));
                        if let Some(prev) = self.current.replace((request, page)) {
                            self.history.push(prev);
                            if self.history.len() > HISTORY_MAX {
                                self.history.remove(0);
                            }
                        }
                        self.show_lyrics = false;
                    }
                    Err(e) => self.set_notice(e),
                }
            }
            Update::Lyrics { video_id, lyrics } => {
                self.lyrics = Some((video_id, lyrics));
                self.lyric_line = None;
                self.lyric_scroll(&mut out);
            }
            Update::AuthRequired(m) => {
                self.logged_in = false;
                let l = self.login.get_or_insert_with(Login::default);
                l.message = Some(m);
            }
            Update::Notice(n) => {
                if n == "Logged in" {
                    self.login = None;
                    self.logged_in = true;
                }
                self.set_notice(n);
            }
        }
        out
    }

    /// Rebuilds the backdrop when the current track's art is loaded and differs from it.
    fn refresh_backdrop(&mut self) {
        if !self.look.backdrop {
            self.backdrop = None;
            return;
        }
        let Some(url) = self
            .snapshot
            .current()
            .and_then(|t| t.thumbnail_url.as_ref())
        else {
            return; // keep the last backdrop while nothing plays
        };
        if self.backdrop.as_ref().is_some_and(|(u, _)| u == url) {
            return;
        }
        if let Some(image::Handle::Bytes(_, bytes)) = self.art.get(url) {
            if let Some(h) = blurred(bytes, &self.look) {
                self.backdrop = Some((url.clone(), h));
            }
        }
    }

    /// Urls not yet loaded or in flight; marks them requested.
    fn want_art(&mut self, urls: impl IntoIterator<Item = Option<String>>) -> Vec<String> {
        let mut v = Vec::new();
        for u in urls.into_iter().flatten() {
            if self.requested.insert(u.clone()) {
                v.push(u);
            }
        }
        v
    }

    /// Lyrics of the current track, if they arrived.
    pub fn current_lyrics(&self) -> Option<&Lyrics> {
        let id = &self.snapshot.current()?.video_id;
        match &self.lyrics {
            Some((v, Some(l))) if v == id => Some(l),
            _ => None,
        }
    }
}

pub fn thumb(it: &Item) -> Option<String> {
    match it {
        Item::Track(t) => t.thumbnail_url.clone(),
        Item::Playlist(e) | Item::Album(e) | Item::Artist(e) => e.thumbnail_url.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmusic_core::Entry;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: "a".into(),
            album: None,
            duration_secs: Some(10),
            thumbnail_url: None,
        }
    }

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            thumbnail_url: None,
        }
    }

    fn state() -> State {
        State::new(Size::new(1100.0, 700.0))
    }

    fn page_update(req: PageRequest, items: Vec<Item>) -> Message {
        Message::Engine(Update::Page {
            request: req,
            result: Ok(Page {
                title: "t".into(),
                items,
            }),
        })
    }

    #[test]
    fn search_submit_fetches() {
        let mut s = state();
        s.update(Message::SearchInput("  daft punk ".into()));
        let out = s.update(Message::SearchSubmit);
        assert_eq!(
            out.commands,
            vec![Command::Fetch(PageRequest::Search("daft punk".into()))]
        );
        assert_eq!(s.awaiting, Some(PageRequest::Search("daft punk".into())));
    }

    #[test]
    fn page_matching_fills_mismatched_ignored() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(PageRequest::LibrarySongs, vec![]));
        assert!(s.current.is_none());
        assert!(s.awaiting.is_some());
        s.update(page_update(
            PageRequest::LikedSongs,
            vec![Item::Track(track("x"))],
        ));
        assert_eq!(s.current.as_ref().unwrap().1.items.len(), 1);
        assert!(s.awaiting.is_none());
    }

    #[test]
    fn play_row_starts_at_track_index() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(
            PageRequest::LikedSongs,
            vec![
                Item::Album(entry("al")),
                Item::Track(track("a")),
                Item::Artist(entry("ar")),
                Item::Track(track("b")),
            ],
        ));
        let out = s.update(Message::PlayRow(3));
        assert_eq!(
            out.commands,
            vec![Command::PlayTracks {
                tracks: vec![track("a"), track("b")],
                start: 1
            }]
        );
        assert!(s.update(Message::PlayRow(0)).commands.is_empty());
    }

    #[test]
    fn back_restores_previous_page() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(
            PageRequest::LikedSongs,
            vec![Item::Track(track("a"))],
        ));
        s.update(Message::Open(PageRequest::Album("x".into())));
        s.update(page_update(PageRequest::Album("x".into()), vec![]));
        let out = s.update(Message::Back);
        assert!(out.commands.is_empty());
        assert_eq!(s.current.as_ref().unwrap().0, PageRequest::LikedSongs);
    }

    #[test]
    fn seek_drag_sends_one_seek_on_release() {
        let mut s = state();
        let mut all = vec![];
        for m in [
            Message::SeekChanged(1.0),
            Message::SeekChanged(2.0),
            Message::SeekChanged(30.0),
            Message::SeekReleased,
            Message::SeekReleased,
        ] {
            all.extend(s.update(m).commands);
        }
        assert_eq!(all, vec![Command::SeekTo(30.0)]);
        assert!(s.drag.is_none());
    }

    #[test]
    fn login_opens_on_auth_and_closes_on_logged_in() {
        let mut s = state();
        s.update(Message::Engine(Update::AuthRequired("expired".into())));
        assert_eq!(
            s.login.as_ref().unwrap().message.as_deref(),
            Some("expired")
        );
        s.logged_in = true;
        s.update(Message::Engine(Update::AuthRequired("expired".into())));
        assert!(!s.logged_in);
        s.update(Message::Engine(Update::Notice("Logged in".into())));
        assert!(s.login.is_none());
        assert!(s.logged_in);
    }

    #[test]
    fn like_toggles_and_liked_page_fills() {
        let mut s = state();
        let rate = |r| Command::Rate {
            video_id: "a".into(),
            rating: r,
        };
        assert_eq!(
            s.update(Message::Like("a".into())).commands,
            vec![rate(Rating::Like)]
        );
        assert!(s.liked.contains("a"));
        assert_eq!(
            s.update(Message::Like("a".into())).commands,
            vec![rate(Rating::None)]
        );
        assert!(s.liked.is_empty());
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(
            PageRequest::LikedSongs,
            vec![Item::Track(track("x")), Item::Album(entry("al"))],
        ));
        assert_eq!(s.liked, HashSet::from(["x".to_string()]));
    }

    #[test]
    fn trim_bars_crops_black_bands_only() {
        let enc = |img: ::image::RgbImage| {
            let mut o = std::io::Cursor::new(Vec::new());
            img.write_to(&mut o, ::image::ImageFormat::Png).unwrap();
            o.into_inner()
        };
        let dim = |b: &[u8]| ::image::load_from_memory(b).map(|i| (i.width(), i.height()));
        let bars = ::image::RgbImage::from_fn(30, 30, |_, y| {
            ::image::Rgb(if (4..26).contains(&y) {
                [200, 90, 30]
            } else {
                [0, 0, 0]
            })
        });
        assert_eq!(dim(&trim_bars(enc(bars))).unwrap(), (22, 22));
        let full = enc(::image::RgbImage::from_pixel(
            40,
            30,
            ::image::Rgb([200, 90, 30]),
        ));
        assert_eq!(trim_bars(full.clone()), full);
        let wide = enc(::image::RgbImage::new(40, 20));
        assert_eq!(trim_bars(wide.clone()), wide);
    }

    fn playing(s: &mut State, id: &str, pos: f64) {
        s.update(Message::Engine(Update::Player(Snapshot {
            queue: vec![track(id)],
            cursor: Some(0),
            position_secs: pos,
            volume: 98,
            ..Snapshot::default()
        })));
    }

    #[test]
    fn clicking_the_playing_track_toggles_pause() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(
            PageRequest::LikedSongs,
            vec![Item::Track(track("a")), Item::Track(track("b"))],
        ));
        playing(&mut s, "b", 0.0);
        assert_eq!(
            s.update(Message::PlayRow(1)).commands,
            vec![Command::TogglePause]
        );
        assert_eq!(s.update(Message::PlayRow(0)).commands.len(), 1);
        assert!(matches!(
            s.update(Message::PlayRow(0)).commands[0],
            Command::PlayTracks { start: 0, .. }
        ));
    }

    #[test]
    fn escape_closes_login_then_lyrics_then_goes_back() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(PageRequest::LikedSongs, vec![]));
        s.update(Message::Open(PageRequest::LibrarySongs));
        s.update(page_update(PageRequest::LibrarySongs, vec![]));
        s.login = Some(Login::default());
        s.show_lyrics = true;
        s.update(Message::Escape);
        assert!(s.login.is_none() && s.show_lyrics);
        s.update(Message::Escape);
        assert!(!s.show_lyrics && s.history.len() == 1);
        let out = s.update(Message::Escape);
        assert!(s.history.is_empty());
        assert_eq!(out.scroll, Some(Scroll::ListTop));
        assert_eq!(s.current.as_ref().unwrap().0, PageRequest::LikedSongs);
    }

    #[test]
    fn keys_map_to_commands() {
        let mut s = state();
        playing(&mut s, "a", 0.0);
        assert_eq!(
            s.update(Message::SeekBy(-5.0)).commands,
            vec![Command::SeekBy(-5.0)]
        );
        assert_eq!(
            s.update(Message::VolumeBy(5)).commands,
            vec![Command::SetVolume(100)]
        );
        assert_eq!(
            s.update(Message::VolumeBy(-5)).commands,
            vec![Command::SetVolume(93)]
        );
        assert!(s.update(Message::FocusSearch).focus_search);
    }

    #[test]
    fn notice_clears_only_for_its_own_generation() {
        let mut s = state();
        let n = |s: &mut State, t: &str| {
            s.update(Message::Engine(Update::Notice(t.into())))
                .notice_gen
                .unwrap()
        };
        let (a, b) = (n(&mut s, "one"), n(&mut s, "two"));
        assert_ne!(a, b);
        s.update(Message::ClearNotice(a));
        assert_eq!(s.notice.as_deref(), Some("two"));
        s.update(Message::ClearNotice(b));
        assert_eq!(s.notice, None);
    }

    #[test]
    fn lyrics_follow_the_current_line_unless_the_user_scrolled() {
        let mut s = state();
        s.show_lyrics = true;
        let lines = (0..20)
            .map(|i| (Some(i as f64 * 10.0), format!("l{i}")))
            .collect();
        playing(&mut s, "a", 0.0);
        let at = |s: &mut State, pos: f64| {
            s.update(Message::Engine(Update::Player(Snapshot {
                position_secs: pos,
                ..s.snapshot.clone()
            })))
            .scroll
        };
        let out = s.update(Message::Engine(Update::Lyrics {
            video_id: "a".into(),
            lyrics: Some(Lyrics { lines }),
        }));
        assert_eq!(out.scroll, Some(Scroll::Lyrics(0.0)));
        s.update(Message::LyricsScrolled(400.0)); // echo of our own scroll
        let pad = s.look.pad;
        let centred = move |i: f32| (i + 0.5) * LYRIC_LH + pad - 200.0;
        assert_eq!(at(&mut s, 5.0), None); // same line
        let want = centred(10.0);
        assert_eq!(at(&mut s, 100.0), Some(Scroll::Lyrics(want)));
        s.update(Message::LyricsScrolled(400.0)); // echo
        s.update(Message::LyricsScrolled(400.0)); // the user
        s.now += Duration::from_secs(1);
        assert_eq!(at(&mut s, 110.0), None);
        s.now += Duration::from_secs(6);
        let want = centred(12.0);
        assert_eq!(at(&mut s, 120.0), Some(Scroll::Lyrics(want)));
    }

    #[test]
    fn close_sends_quit() {
        let mut s = state();
        let out = s.update(Message::CloseRequested);
        assert_eq!(out.commands, vec![Command::Quit]);
        assert!(out.closing);
    }
}
