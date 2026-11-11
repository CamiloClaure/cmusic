//! State, messages and the pure update logic. `State::update` never touches the engine or the
//! network: it returns an `Out` describing what the thin wrapper in `main.rs` must do.

use crate::theme::{self, Look};
use crate::view;
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
/// Decoded art kept before `trim_art` drops what is off screen.
const ART_MAX: usize = 120;
/// Rows of a page whose streams are resolved ahead of a click.
const WARM_ROWS: usize = 3;
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
    Video,
    ToggleLyrics,
    ToggleQueue,
    FocusSearch,
    Escape,
    /// The lyrics scrollable moved: viewport height, offset and largest possible offset.
    LyricsScrolled(f32, f32, f32),
    /// Clears the notice if it is still the one with this generation.
    ClearNotice(u64),
    Jump(usize),
    Remove(usize),
    ClearQueue,
    OpenLogin,
    CloseLogin,
    Logout,
    ImportBrowsers,
    CookieInput(String),
    UseCookie,
    Art(String, Option<Vec<u8>>),
    Resized(Size),
    CloseRequested,
    CycleTheme,
    ReloadTheme,
    /// Debug script driver (see `main.rs`): advance, or time out the page wait of that id.
    Demo(Option<u64>),
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
    /// The last request that failed and its error; shown in place of the page until the next fetch.
    pub failed: Option<(PageRequest, String)>,
    pub current: Option<(PageRequest, Page)>,
    pub history: Vec<(PageRequest, Page)>,
    pub lyrics: Option<(String, Option<Lyrics>)>,
    pub show_lyrics: bool,
    /// Narrow windows only: the queue replaces the main area.
    pub queue_open: bool,
    /// Set by the wrapper before each update, so `update` stays deterministic.
    pub now: Instant,
    notice_gen: u64,
    /// How long the current notice stays, in seconds (read by the wrapper).
    pub notice_secs: u64,
    /// The notice is the error of a failed page: the next page that loads clears it.
    page_err: bool,
    lyric_line: Option<usize>,
    user_scrolled: Option<Instant>,
    /// Offset we last asked the lyrics view for, and the last one it reported.
    lyric_target: Option<f32>,
    lyric_y: Option<f32>,
    lyrics_h: f32,
    pub login: Option<Login>,
    pub notice: Option<String>,
    /// Set at startup from the cookie file, then by the startup check (`Session`), the "Logged in" notice and `AuthRequired`.
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
            failed: None,
            current: None,
            history: Vec::new(),
            lyrics: None,
            show_lyrics: false,
            queue_open: false,
            now: Instant::now(),
            notice_gen: 0,
            notice_secs: 5,
            page_err: false,
            lyric_line: None,
            user_scrolled: None,
            lyric_target: None,
            lyric_y: None,
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

    /// Long notices (errors, mostly) stay up longer, 15 s at most.
    fn set_notice(&mut self, n: String) {
        self.notice_secs = (5 + n.chars().count() as u64 / 10).min(15);
        self.notice = Some(n);
        self.notice_gen += 1;
        self.page_err = false;
    }

    fn step(&mut self, msg: Message) -> Out {
        let mut out = Out::default();
        match msg {
            Message::Engine(u) => return self.engine(u),
            Message::EngineStopped => out.stopped = true,
            Message::Demo(_) => {}
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
            Message::LyricsScrolled(h, y, max) => {
                self.lyrics_h = h;
                // Only a move to somewhere we did not ask for is the user's: the first report of a
                // fresh view and the echo of our own scroll (clamped at the end) are not.
                let ours = self
                    .lyric_target
                    .is_some_and(|t| (t.min(max) - y).abs() < 2.0);
                if self.lyric_y.is_some_and(|p| (p - y).abs() >= 1.0) && !ours {
                    self.user_scrolled = Some(self.now);
                }
                self.lyric_y = Some(y);
            }
            Message::ClearNotice(g) => {
                if g == self.notice_gen {
                    self.notice = None;
                }
            }
            Message::SeekBy(d) => {
                self.user_scrolled = None;
                out.commands.push(Command::SeekBy(d));
            }
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
            Message::Next => {
                self.user_scrolled = None;
                out.commands.push(Command::Next);
            }
            Message::Prev => {
                self.user_scrolled = None;
                out.commands.push(Command::Prev);
            }
            Message::SeekChanged(v) => self.drag = Some(v),
            Message::SeekReleased => {
                if let Some(v) = self.drag.take() {
                    self.user_scrolled = None;
                    out.commands.push(Command::SeekTo(v));
                }
            }
            Message::Volume(v) => out.commands.push(Command::SetVolume(v)),
            Message::Shuffle => out.commands.push(Command::ToggleShuffle),
            Message::Repeat => out.commands.push(Command::CycleRepeat),
            Message::Normalize => out.commands.push(Command::ToggleNormalize),
            Message::Video => out.commands.push(Command::SetVideo(!self.snapshot.video)),
            Message::ToggleLyrics => {
                self.show_lyrics = !self.show_lyrics;
                self.lyric_line = None;
                self.lyric_y = None;
                self.user_scrolled = None;
                self.lyric_scroll(&mut out);
            }
            Message::Jump(i) => {
                self.user_scrolled = None;
                out.commands.push(Command::JumpTo(i));
            }
            Message::Remove(i) => out.commands.push(Command::RemoveFromQueue(i)),
            Message::ClearQueue => out.commands.push(Command::ClearQueue),
            Message::OpenLogin => {
                self.login = match self.login {
                    Some(_) => None,
                    None => Some(Login::default()),
                }
            }
            Message::CloseLogin => self.login = None,
            Message::Logout => out.commands.push(Command::Logout),
            Message::ImportBrowsers => {
                self.checking();
                out.commands
                    .push(Command::ImportCookies(CookieSource::Browsers));
            }
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
                        self.checking();
                    }
                }
            }
            Message::Art(url, bytes) => {
                if let Some(b) = bytes {
                    self.art.insert(url, image::Handle::from_bytes(b));
                    self.trim_art();
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
        self.login = None;
        if let Some(prev) = self.history.pop() {
            self.current = Some(prev);
            self.awaiting = None;
            self.failed = None;
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
        // Window minus outer and panel padding, the bar (two rows when narrow), the header and the
        // content padding; only until the view reports its real height.
        let bar = if self.size.width < NARROW { 96.0 } else { 64.0 };
        let est = self.size.height - 7.0 * l.gap - bar - 36.0 - l.pad;
        let h = if self.lyrics_h > 0.0 {
            self.lyrics_h
        } else {
            est
        };
        let ly = self.current_lyrics().expect("line found above");
        let y = (view::lyric_centre(l, ly, i, view::lyric_w(self)) + l.pad - h / 2.0).max(0.0);
        self.lyric_target = Some(y);
        out.scroll = Some(Scroll::Lyrics(y));
    }

    fn checking(&mut self) {
        if let Some(l) = &mut self.login {
            l.message = Some("Checking the login with YouTube...".into());
        }
    }

    fn fetch(&mut self, r: PageRequest, out: &mut Out) {
        // Navigating closes the login panel; a page that needs a login opens it again.
        self.login = None;
        self.awaiting = Some(r.clone());
        self.failed = None;
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
                    self.user_scrolled = None;
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
                        if std::mem::take(&mut self.page_err) {
                            self.notice = None;
                        }
                        if request == PageRequest::LikedSongs {
                            self.liked
                                .extend(page.items.iter().filter_map(|it| match it {
                                    Item::Track(t) => Some(t.video_id.clone()),
                                    _ => None,
                                }));
                        }
                        out.art = self.want_art(page.items.iter().take(ART_ROWS).map(thumb));
                        // ponytail: the first rows of a fresh page only; no hover warming.
                        let warm: Vec<String> = page
                            .items
                            .iter()
                            .filter_map(|it| match it {
                                Item::Track(t) => Some(t.video_id.clone()),
                                _ => None,
                            })
                            .take(WARM_ROWS)
                            .collect();
                        if !warm.is_empty() {
                            out.commands.push(Command::Warm(warm));
                        }
                        if let Some(prev) = self.current.replace((request, page)) {
                            self.history.push(prev);
                            if self.history.len() > HISTORY_MAX {
                                self.history.remove(0);
                            }
                        }
                        self.show_lyrics = false;
                    }
                    Err(e) => {
                        self.failed = Some((request, e.clone()));
                        self.set_notice(e);
                        self.page_err = true;
                    }
                }
            }
            Update::Lyrics { video_id, lyrics } => {
                self.lyrics = Some((video_id, lyrics));
                self.lyric_line = None;
                self.user_scrolled = None;
                // A fresh list starts at the top until a line is current.
                self.lyric_target = Some(0.0);
                out.scroll = Some(Scroll::Lyrics(0.0));
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
                    // The page that asked for the login (or was loading) loads by itself.
                    let pending = self.failed.take().map(|(r, _)| r).or(self.awaiting.take());
                    if let Some(r) = pending {
                        self.fetch(r, &mut out);
                    }
                }
                self.set_notice(n);
            }
            Update::Session(ok) => match ok {
                Some(v) => self.logged_in = v,
                // Saved but unverified: keep the cookies, close the form.
                None => {
                    self.login = None;
                    self.logged_in = true;
                }
            },
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

    /// Keeps the art cache bounded: past `ART_MAX` images, only art that is on screen (page rows,
    /// queue, current track, backdrop) stays. Dropped urls may be fetched again when they return.
    fn trim_art(&mut self) {
        if self.art.len() <= ART_MAX {
            return;
        }
        let mut keep: HashSet<String> = self
            .snapshot
            .queue
            .iter()
            .filter_map(|t| t.thumbnail_url.clone())
            .collect();
        if let Some((_, page)) = &self.current {
            keep.extend(page.items.iter().take(ART_ROWS).filter_map(thumb));
        }
        keep.extend(self.backdrop.as_ref().map(|(u, _)| u.clone()));
        let requested = &mut self.requested;
        self.art.retain(|u, _| {
            let k = keep.contains(u);
            if !k {
                requested.remove(u);
            }
            k
        });
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
    fn art_cache_keeps_only_on_screen_art_past_the_cap() {
        let mut s = state();
        let mut t = track("x");
        t.thumbnail_url = Some("u0".into());
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(page_update(PageRequest::LikedSongs, vec![Item::Track(t)]));
        for i in 0..=ART_MAX {
            let u = format!("u{i}");
            s.want_art([Some(u.clone())]);
            s.update(Message::Art(u, Some(vec![0])));
        }
        // Over the cap: only the page row's art survives, and the rest may be fetched again.
        assert_eq!(s.art.keys().collect::<Vec<_>>(), vec!["u0"]);
        assert_eq!(s.want_art([Some("u5".into())]), vec!["u5".to_string()]);
        assert!(s.want_art([Some("u0".into())]).is_empty());
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
    fn failed_page_keeps_its_error_until_the_next_fetch() {
        let mut s = state();
        s.update(Message::Open(PageRequest::LikedSongs));
        s.update(Message::Engine(Update::Page {
            request: PageRequest::LikedSongs,
            result: Err("boom".into()),
        }));
        assert_eq!(s.failed.as_ref().unwrap().1, "boom");
        assert!(s.current.is_none() && s.awaiting.is_none());
        s.update(Message::Open(PageRequest::LibrarySongs));
        assert!(s.failed.is_none());
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
    fn successful_login_refetches_the_pending_page() {
        let mut s = state();
        let req = PageRequest::LibraryPlaylists;
        s.update(Message::Open(req.clone()));
        s.update(Message::Engine(Update::Page {
            request: req.clone(),
            result: Err("log in".into()),
        }));
        s.update(Message::Engine(Update::AuthRequired("log in".into())));
        assert!(s.login.is_some());
        s.update(Message::CookieInput("SID=a".into()));
        s.update(Message::UseCookie);
        assert_eq!(
            s.login.as_ref().unwrap().message.as_deref(),
            Some("Checking the login with YouTube...")
        );
        // A rejected attempt keeps the form open and shows the error in it.
        s.update(Message::Engine(Update::AuthRequired("nope".into())));
        assert_eq!(s.login.as_ref().unwrap().message.as_deref(), Some("nope"));
        let out = s.update(Message::Engine(Update::Notice("Logged in".into())));
        assert!(s.login.is_none());
        assert_eq!(out.commands, vec![Command::Fetch(req.clone())]);
        assert_eq!(s.awaiting, Some(req));
        // A dead stored session found at startup only flips the sidebar button.
        s.update(Message::Engine(Update::Session(Some(false))));
        assert!(!s.logged_in && s.login.is_none());
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

    fn synced(s: &mut State) -> impl FnMut(&mut State, f64) -> Option<Scroll> {
        s.show_lyrics = true;
        let lines = (0..20)
            .map(|i| (Some(i as f64 * 10.0), format!("l{i}")))
            .collect();
        playing(s, "a", 0.0);
        s.update(Message::Engine(Update::Lyrics {
            video_id: "a".into(),
            lyrics: Some(Lyrics { lines }),
        }));
        |s: &mut State, pos: f64| {
            s.update(Message::Engine(Update::Player(Snapshot {
                position_secs: pos,
                ..s.snapshot.clone()
            })))
            .scroll
        }
    }

    #[test]
    fn lyrics_follow_the_current_line_unless_the_user_scrolled() {
        let mut s = state();
        let mut at = synced(&mut s);
        let want = |s: &State, i: usize| {
            let ly = s.current_lyrics().unwrap();
            let est = 400.0; // the height the view reported
            Some(Scroll::Lyrics(
                (view::lyric_centre(s.look, ly, i, view::lyric_w(s)) + s.look.pad - est / 2.0)
                    .max(0.0),
            ))
        };
        s.update(Message::LyricsScrolled(400.0, 0.0, 900.0)); // first report of the view
        assert_eq!(at(&mut s, 5.0), None); // same line
        assert_eq!(at(&mut s, 100.0), want(&s, 10));
        let Some(Scroll::Lyrics(y)) = want(&s, 10) else {
            panic!()
        };
        s.update(Message::LyricsScrolled(400.0, y, 900.0)); // echo of our own scroll
        assert!(s.user_scrolled.is_none());
        s.update(Message::LyricsScrolled(400.0, y + 90.0, 900.0)); // the user
        s.now += Duration::from_secs(1);
        assert_eq!(at(&mut s, 110.0), None);
        s.now += Duration::from_secs(6);
        assert_eq!(at(&mut s, 120.0), want(&s, 12));
    }

    #[test]
    fn opening_lyrics_and_seeking_are_not_manual_scrolls() {
        let mut s = state();
        let mut at = synced(&mut s);
        s.update(Message::ToggleLyrics); // hide
        let out = s.update(Message::ToggleLyrics); // show
        s.update(Message::LyricsScrolled(400.0, 0.0, 900.0)); // fresh view reports offset 0
        let Some(Scroll::Lyrics(_)) = out.scroll else {
            panic!("no scroll on open")
        };
        assert!(s.user_scrolled.is_none());
        // A real scroll holds following, but a seek releases it at once.
        s.update(Message::LyricsScrolled(400.0, 123.0, 900.0));
        assert!(s.user_scrolled.is_some());
        s.update(Message::SeekChanged(100.0));
        s.update(Message::SeekReleased);
        assert!(at(&mut s, 100.0).is_some());
    }

    #[test]
    fn untimed_lyrics_never_scroll() {
        let mut s = state();
        s.show_lyrics = true;
        playing(&mut s, "a", 50.0);
        let lines = vec![(None, "x".to_string()), (None, "y".to_string())];
        let out = s.update(Message::Engine(Update::Lyrics {
            video_id: "a".into(),
            lyrics: Some(Lyrics { lines }),
        }));
        assert_eq!(out.scroll, Some(Scroll::Lyrics(0.0)));
        assert_eq!(s.lyric_line, None);
    }

    #[test]
    fn navigation_closes_login_and_page_error_notice_clears() {
        let mut s = state();
        s.update(Message::OpenLogin);
        assert!(s.login.is_some());
        s.update(Message::SearchInput("bach".into()));
        s.update(Message::SearchSubmit);
        assert!(s.login.is_none());
        s.update(Message::OpenLogin);
        s.update(Message::OpenLogin);
        assert!(s.login.is_none(), "the button toggles");
        s.update(Message::Engine(Update::Page {
            request: PageRequest::Search("bach".into()),
            result: Err("search broke".into()),
        }));
        assert_eq!(s.notice.as_deref(), Some("search broke"));
        s.update(Message::Open(PageRequest::Search("x".into())));
        s.update(page_update(PageRequest::Search("x".into()), vec![]));
        assert_eq!(s.notice, None);
        // Other notices stay, and long ones last longer (15 s at most).
        s.update(Message::Engine(Update::Notice("Hi".into())));
        s.update(page_update(PageRequest::Search("x".into()), vec![]));
        assert_eq!(s.notice_secs, 5);
        s.update(Message::Engine(Update::Notice("e".repeat(200))));
        assert_eq!(s.notice_secs, 15);
    }

    #[test]
    fn logout_sends_the_command() {
        let mut s = state();
        assert_eq!(s.update(Message::Logout).commands, vec![Command::Logout]);
    }

    #[test]
    fn close_sends_quit() {
        let mut s = state();
        let out = s.update(Message::CloseRequested);
        assert_eq!(out.commands, vec![Command::Quit]);
        assert!(out.closing);
    }
}
