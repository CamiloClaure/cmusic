//! UI state. `on_key` and `on_update` touch neither the terminal nor the engine.

use cmusic_core::{Command, Item, Lyrics, Page, PageRequest, Rating, Snapshot, Track, Update};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Main,
    Queue,
}

pub const SIDEBAR: [&str; 6] = [
    "Search",
    "Liked songs",
    "Playlists",
    "Songs",
    "Albums",
    "Artists",
];

const PAGE_STEP: usize = 10;
const HISTORY_MAX: usize = 20;

pub struct App {
    pub snap: Snapshot,
    pub focus: Focus,
    pub sidebar_sel: usize,
    pub page: Option<Page>,
    pub main_sel: usize,
    /// Pages already loaded, with their selection, for Backspace.
    history: Vec<(Page, usize)>,
    /// The request whose answer we are waiting for; any other `Page` update is stale.
    pub awaiting: Option<PageRequest>,
    pub queue_sel: usize,
    pub lyrics: Option<(String, Lyrics)>,
    pub notice: String,
    /// `Some` while the search box is open.
    pub input: Option<String>,
    pub help: bool,
    pub quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            snap: Snapshot::default(),
            focus: Focus::Sidebar,
            sidebar_sel: 0,
            page: None,
            main_sel: 0,
            history: Vec::new(),
            awaiting: None,
            queue_sel: 0,
            lyrics: None,
            notice: String::new(),
            input: None,
            help: false,
            quit: false,
        }
    }
}

impl App {
    fn len(&self, focus: Focus) -> usize {
        match focus {
            Focus::Sidebar => SIDEBAR.len(),
            Focus::Main => self.page.as_ref().map_or(0, |p| p.items.len()),
            Focus::Queue => self.snap.queue.len(),
        }
    }

    fn sel_mut(&mut self) -> &mut usize {
        match self.focus {
            Focus::Sidebar => &mut self.sidebar_sel,
            Focus::Main => &mut self.main_sel,
            Focus::Queue => &mut self.queue_sel,
        }
    }

    fn fetch(&mut self, req: PageRequest) -> Vec<Command> {
        self.awaiting = Some(req.clone());
        self.notice.clear();
        vec![Command::Fetch(req)]
    }

    fn selected_track(&self) -> Option<&Track> {
        match self.page.as_ref()?.items.get(self.main_sel)? {
            Item::Track(t) => Some(t),
            _ => None,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.quit();
        }
        if self.input.is_some() {
            return self.on_input_key(key);
        }
        if ctrl {
            return vec![];
        }
        if self.help {
            self.help = false;
            return vec![];
        }
        let vol = self.snap.volume;
        match key.code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('/') => self.input = Some(String::new()),
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(2),
            KeyCode::Char('h') | KeyCode::Left if !shift => self.move_focus(2),
            KeyCode::Char('l') | KeyCode::Right if !shift => self.move_focus(1),
            KeyCode::Right => return vec![Command::SeekBy(30.0)],
            KeyCode::Left => return vec![Command::SeekBy(-30.0)],
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown => self.step(PAGE_STEP as isize),
            KeyCode::PageUp => self.step(-(PAGE_STEP as isize)),
            KeyCode::Char('g') | KeyCode::Home => *self.sel_mut() = 0,
            KeyCode::Char('G') | KeyCode::End => {
                *self.sel_mut() = self.len(self.focus).saturating_sub(1)
            }
            KeyCode::Char(' ') => return vec![Command::TogglePause],
            KeyCode::Char('>') => return vec![Command::Next],
            KeyCode::Char('<') => return vec![Command::Prev],
            KeyCode::Char('.') => return vec![Command::SeekBy(5.0)],
            KeyCode::Char(',') => return vec![Command::SeekBy(-5.0)],
            KeyCode::Char('=') => return vec![Command::SetVolume(vol.saturating_add(5).min(100))],
            KeyCode::Char('_') => return vec![Command::SetVolume(vol.saturating_sub(5))],
            KeyCode::Char('s') => return vec![Command::ToggleShuffle],
            KeyCode::Char('r') => return vec![Command::CycleRepeat],
            KeyCode::Char('N') => return vec![Command::ToggleNormalize],
            _ => return self.on_pane_key(key.code),
        }
        vec![]
    }

    fn on_pane_key(&mut self, code: KeyCode) -> Vec<Command> {
        match (self.focus, code) {
            (Focus::Sidebar, KeyCode::Enter) => match self.sidebar_sel {
                0 => self.input = Some(String::new()),
                i => {
                    let req = [
                        PageRequest::LikedSongs,
                        PageRequest::LibraryPlaylists,
                        PageRequest::LibrarySongs,
                        PageRequest::LibraryAlbums,
                        PageRequest::LibraryArtists,
                    ][i - 1]
                        .clone();
                    self.focus = Focus::Main;
                    return self.fetch(req);
                }
            },
            (Focus::Main, KeyCode::Enter) => return self.open_selected(),
            (Focus::Main, KeyCode::Backspace) => {
                if let Some((page, sel)) = self.history.pop() {
                    self.page = Some(page);
                    self.main_sel = sel;
                    self.awaiting = None;
                }
            }
            (Focus::Main, KeyCode::Char(c @ ('a' | 'n' | '+' | '-'))) => {
                let Some(t) = self.selected_track().cloned() else {
                    return vec![];
                };
                return vec![match c {
                    'a' => Command::Enqueue(vec![t]),
                    'n' => Command::PlayNext(vec![t]),
                    _ => Command::Rate {
                        video_id: t.video_id,
                        rating: if c == '+' {
                            Rating::Like
                        } else {
                            Rating::Dislike
                        },
                    },
                }];
            }
            (Focus::Queue, KeyCode::Enter) if self.queue_sel < self.snap.queue.len() => {
                return vec![Command::JumpTo(self.queue_sel)];
            }
            (Focus::Queue, KeyCode::Char('d')) if self.queue_sel < self.snap.queue.len() => {
                return vec![Command::RemoveFromQueue(self.queue_sel)];
            }
            (Focus::Queue, KeyCode::Char('c' | 'C')) => return vec![Command::ClearQueue],
            _ => {}
        }
        vec![]
    }

    fn open_selected(&mut self) -> Vec<Command> {
        let Some(page) = &self.page else {
            return vec![];
        };
        let req = match page.items.get(self.main_sel) {
            None => return vec![],
            Some(Item::Track(_)) => {
                let tracks: Vec<Track> = page
                    .items
                    .iter()
                    .filter_map(|i| match i {
                        Item::Track(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect();
                let start = page.items[..self.main_sel]
                    .iter()
                    .filter(|i| matches!(i, Item::Track(_)))
                    .count();
                return vec![Command::PlayTracks { tracks, start }];
            }
            Some(Item::Playlist(e)) => PageRequest::Playlist(e.id.clone()),
            Some(Item::Album(e)) => PageRequest::Album(e.id.clone()),
            Some(Item::Artist(e)) => PageRequest::Artist(e.id.clone()),
        };
        self.fetch(req)
    }

    fn on_input_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let Some(q) = self.input.as_mut() else {
            return vec![];
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                q.pop();
            }
            KeyCode::Char(c) => q.push(c),
            KeyCode::Enter => {
                let q = q.trim().to_string();
                self.input = None;
                if !q.is_empty() {
                    self.focus = Focus::Main;
                    return self.fetch(PageRequest::Search(q));
                }
            }
            _ => {}
        }
        vec![]
    }

    fn quit(&mut self) -> Vec<Command> {
        self.quit = true;
        vec![Command::Quit]
    }

    /// `by` is 1 (forward) or 2 (backward) of 3 panes.
    fn move_focus(&mut self, by: usize) {
        let all = [Focus::Sidebar, Focus::Main, Focus::Queue];
        let i = all.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = all[(i + by) % 3];
    }

    fn step(&mut self, delta: isize) {
        let last = self.len(self.focus).saturating_sub(1);
        let sel = self.sel_mut();
        *sel = sel.saturating_add_signed(delta).min(last);
    }

    pub fn on_update(&mut self, update: Update) {
        match update {
            Update::Player(s) => {
                self.snap = s;
                self.queue_sel = self.queue_sel.min(self.snap.queue.len().saturating_sub(1));
            }
            Update::Page { request, result } => {
                if self.awaiting.as_ref() != Some(&request) {
                    return;
                }
                self.awaiting = None;
                match result {
                    Ok(page) => {
                        if let Some(old) = self.page.replace(page) {
                            self.history.push((old, self.main_sel));
                            if self.history.len() > HISTORY_MAX {
                                self.history.remove(0);
                            }
                        }
                        self.main_sel = 0;
                    }
                    Err(m) => self.notice = m,
                }
            }
            Update::Lyrics { video_id, lyrics } => {
                self.lyrics = lyrics.map(|l| (video_id, l));
            }
            Update::AuthRequired(m) => self.notice = format!("{m} (run: cmusic login)"),
            Update::Notice(m) => self.notice = m,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cmusic_core::Entry;

    pub fn track(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Artist".into(),
            album: None,
            duration_secs: Some(100),
            thumbnail_url: None,
        }
    }

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            title: format!("Album {id}"),
            subtitle: String::new(),
            thumbnail_url: None,
        }
    }

    fn press(app: &mut App, c: char) -> Vec<Command> {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn code(app: &mut App, c: KeyCode) -> Vec<Command> {
        app.on_key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    fn page(title: &str, items: Vec<Item>) -> Page {
        Page {
            title: title.into(),
            items,
        }
    }

    fn deliver(app: &mut App, req: PageRequest, p: Page) {
        app.on_update(Update::Page {
            request: req,
            result: Ok(p),
        });
    }

    #[test]
    fn search_flow() {
        let mut app = App::default();
        press(&mut app, '/');
        for c in "q x".chars() {
            assert!(press(&mut app, c).is_empty());
        }
        let cmds = code(&mut app, KeyCode::Enter);
        let req = PageRequest::Search("q x".into());
        assert_eq!(cmds, vec![Command::Fetch(req.clone())]);
        assert_eq!(app.focus, Focus::Main);

        deliver(
            &mut app,
            PageRequest::Search("other".into()),
            page("stale", vec![]),
        );
        assert!(app.page.is_none());
        deliver(
            &mut app,
            req.clone(),
            page("ok", vec![Item::Track(track("a"))]),
        );
        assert_eq!(app.page.as_ref().unwrap().title, "ok");
        // answered once; a repeat is stale
        deliver(&mut app, req, page("again", vec![]));
        assert_eq!(app.page.as_ref().unwrap().title, "ok");
    }

    #[test]
    fn input_swallows_hotkeys_and_esc_cancels() {
        let mut app = App::default();
        press(&mut app, '/');
        assert!(press(&mut app, 'q').is_empty());
        assert!(!app.quit);
        code(&mut app, KeyCode::Esc);
        assert!(app.input.is_none());
    }

    #[test]
    fn enter_track_among_entries_and_back() {
        let mut app = App::default();
        let req = PageRequest::Search("x".into());
        app.awaiting = Some(req.clone());
        let items = vec![
            Item::Album(entry("al")),
            Item::Track(track("a")),
            Item::Playlist(entry("pl")),
            Item::Track(track("b")),
        ];
        deliver(&mut app, req, page("res", items));
        app.focus = Focus::Main;
        app.main_sel = 3;
        match &code(&mut app, KeyCode::Enter)[..] {
            [Command::PlayTracks { tracks, start }] => {
                assert_eq!((tracks.len(), *start), (2, 1));
                assert_eq!(tracks[*start].video_id, "b");
            }
            c => panic!("{c:?}"),
        }
        app.main_sel = 0;
        assert_eq!(
            code(&mut app, KeyCode::Enter),
            vec![Command::Fetch(PageRequest::Album("al".into()))]
        );
        deliver(
            &mut app,
            PageRequest::Album("al".into()),
            page("album", vec![]),
        );
        assert_eq!(app.page.as_ref().unwrap().title, "album");
        code(&mut app, KeyCode::Backspace);
        assert_eq!(app.page.as_ref().unwrap().title, "res");
    }

    #[test]
    fn queue_keys() {
        let mut app = App::default();
        app.on_update(Update::Player(Snapshot {
            queue: vec![track("a"), track("b"), track("c")],
            cursor: Some(0),
            ..Default::default()
        }));
        app.focus = Focus::Queue;
        press(&mut app, 'j');
        assert_eq!(code(&mut app, KeyCode::Enter), vec![Command::JumpTo(1)]);
        assert_eq!(press(&mut app, 'd'), vec![Command::RemoveFromQueue(1)]);
        assert_eq!(press(&mut app, 'C'), vec![Command::ClearQueue]);
    }

    #[test]
    fn selection_clamps_when_queue_shrinks() {
        let mut app = App::default();
        let q = |n: usize| Snapshot {
            queue: (0..n).map(|i| track(&i.to_string())).collect(),
            ..Default::default()
        };
        app.on_update(Update::Player(q(5)));
        app.focus = Focus::Queue;
        press(&mut app, 'G');
        assert_eq!(app.queue_sel, 4);
        app.on_update(Update::Player(q(2)));
        assert_eq!(app.queue_sel, 1);
        app.on_update(Update::Player(q(0)));
        assert_eq!(app.queue_sel, 0);
        assert!(press(&mut app, 'd').is_empty());
    }

    #[test]
    fn globals() {
        let mut app = App::default();
        app.snap.volume = 98;
        assert_eq!(press(&mut app, '='), vec![Command::SetVolume(100)]);
        assert_eq!(press(&mut app, '.'), vec![Command::SeekBy(5.0)]);
        assert_eq!(press(&mut app, ' '), vec![Command::TogglePause]);
        let c = app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(c, vec![Command::Quit]);
        assert!(app.quit);
    }
}
