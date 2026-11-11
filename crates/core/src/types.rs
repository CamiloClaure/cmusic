//! The contract between core and the front ends. Change it only with the orchestrator's OK:
//! the TUI, the GUI and the engine are all written against these types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    /// Artist names joined with ", ".
    pub artists: String,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Buffering,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// Everything a front end needs to draw the player. Emitted after every change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub status: Status,
    /// The queue in play order (already shuffled when shuffle is on).
    pub queue: Vec<Track>,
    /// Index into `queue` of the current track.
    pub cursor: Option<usize>,
    pub position_secs: f64,
    pub duration_secs: f64,
    /// 0..=100
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub normalize: bool,
    /// Last playback failure, shown to the user. Cleared when a track starts playing.
    pub last_error: Option<String>,
}

impl Snapshot {
    pub fn current(&self) -> Option<&Track> {
        self.cursor.and_then(|i| self.queue.get(i))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rating {
    Like,
    Dislike,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PageRequest {
    Search(String),
    LibraryPlaylists,
    LibrarySongs,
    LibraryAlbums,
    LibraryArtists,
    LikedSongs,
    /// Playlist id.
    Playlist(String),
    /// Album browse id.
    Album(String),
    /// Artist channel id.
    Artist(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Track(Track),
    Playlist(Entry),
    Album(Entry),
    Artist(Entry),
}

/// A non-track row: something you open, not something you play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What goes into the matching `PageRequest` variant.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lyrics {
    /// Start time in seconds when synced, `None` for plain lyrics.
    pub lines: Vec<(Option<f64>, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieSource {
    /// Try the installed browsers.
    Browsers,
    /// A pasted `Cookie:` header value.
    Header(String),
    /// Path to a Netscape cookies.txt.
    File(std::path::PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // Playback
    /// Replace the queue and start at `start`.
    PlayTracks {
        tracks: Vec<Track>,
        start: usize,
    },
    /// Append to the end of the queue.
    Enqueue(Vec<Track>),
    /// Insert right after the current track.
    PlayNext(Vec<Track>),
    /// Index into `Snapshot::queue`.
    JumpTo(usize),
    RemoveFromQueue(usize),
    ClearQueue,
    TogglePause,
    Next,
    Prev,
    SeekTo(f64),
    SeekBy(f64),
    SetVolume(u8),
    ToggleShuffle,
    CycleRepeat,
    ToggleNormalize,
    // Browsing. Results arrive as `Update::Page` / `Update::Notice`.
    Fetch(PageRequest),
    Rate {
        video_id: String,
        rating: Rating,
    },
    AddToPlaylist {
        playlist_id: String,
        video_id: String,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        video_id: String,
    },
    /// Resolve these video ids ahead of time (the first few rows of a page) so playing one is
    /// instant. Best effort, replaces the previous list, never shown to the user.
    Warm(Vec<String>),
    // Login. Success arrives as `Update::Notice`, failure as `Update::AuthRequired`.
    ImportCookies(CookieSource),
    /// Save state and stop the engine.
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Player(Snapshot),
    Page {
        request: PageRequest,
        result: Result<Page, String>,
    },
    /// Lyrics for the current track. `None` means none were found.
    Lyrics {
        video_id: String,
        lyrics: Option<Lyrics>,
    },
    /// The account call failed with 401/403, or no cookies exist. Carries a message to show.
    AuthRequired(String),
    /// One-line message for a status bar (errors included).
    Notice(String),
}
