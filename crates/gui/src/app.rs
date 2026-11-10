//! State, messages and the pure update logic. `State::update` never touches the engine or the
//! network: it returns an `Out` describing what the thin wrapper in `main.rs` must do.

use crate::theme::{self, Look};
use cmusic_core::{
    Command, CookieSource, Item, Lyrics, Page, PageRequest, Rating, Snapshot, Track, Update,
};
use iced::widget::image;
use iced::Size;
use std::collections::{HashMap, HashSet};

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
    Volume(u8),
    Shuffle,
    Repeat,
    Normalize,
    ToggleLyrics,
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
    pub login: Option<Login>,
    pub notice: Option<String>,
    /// Set at startup from the cookie file, then by the "Logged in" notice and `AuthRequired`.
    pub logged_in: bool,
    /// Local slider value while the seek slider is being dragged.
    pub drag: Option<f64>,
    pub art: HashMap<String, image::Handle>,
    /// Blurred art of the current track (keyed by its url), drawn behind everything.
    pub backdrop: Option<(String, image::Handle)>,
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
            login: None,
            notice: None,
            logged_in: false,
            drag: None,
            art: HashMap::new(),
            backdrop: None,
            requested: HashSet::new(),
            size,
            look: Look::default(),
            theme_name: theme::DEFAULT.into(),
        }
    }

    pub fn update(&mut self, msg: Message) -> Out {
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
            Message::Back => {
                if let Some(prev) = self.history.pop() {
                    self.current = Some(prev);
                    self.awaiting = None;
                    self.show_lyrics = false;
                }
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
                    if matches!(page.items.get(i), Some(Item::Track(_))) {
                        out.commands.push(Command::PlayTracks { tracks, start });
                    }
                }
            }
            Message::PlayNext(t) => out.commands.push(Command::PlayNext(vec![t])),
            Message::Enqueue(t) => out.commands.push(Command::Enqueue(vec![t])),
            Message::Like(video_id) => out.commands.push(Command::Rate {
                video_id,
                rating: Rating::Like,
            }),
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
            Message::ToggleLyrics => self.show_lyrics = !self.show_lyrics,
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
                self.notice = Some(format!("Theme: {}", self.theme_name));
                self.backdrop = None;
                self.refresh_backdrop();
            }
            Err(e) => self.notice = Some(e),
        }
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
            }
            Update::Page { request, result } => {
                if self.awaiting.as_ref() != Some(&request) {
                    return out;
                }
                self.awaiting = None;
                match result {
                    Ok(page) => {
                        out.art = self.want_art(page.items.iter().take(ART_ROWS).map(thumb));
                        if let Some(prev) = self.current.replace((request, page)) {
                            self.history.push(prev);
                            if self.history.len() > HISTORY_MAX {
                                self.history.remove(0);
                            }
                        }
                        self.show_lyrics = false;
                    }
                    Err(e) => self.notice = Some(e),
                }
            }
            Update::Lyrics { video_id, lyrics } => self.lyrics = Some((video_id, lyrics)),
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
                self.notice = Some(n);
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
    fn close_sends_quit() {
        let mut s = state();
        let out = s.update(Message::CloseRequested);
        assert_eq!(out.commands, vec![Command::Quit]);
        assert!(out.closing);
    }
}
