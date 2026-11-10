//! YouTube Music through `ytmapi-rs`, mapped onto our own types so a library change stays here.

use crate::auth;
use crate::types::{Entry, Item, Page, PageRequest, Rating, Track};
use futures::TryStreamExt;
use ytmapi_rs::auth::{noauth::NoAuthToken, AuthToken, BrowserToken};
use ytmapi_rs::common::{
    AlbumID, ArtistChannelID, LikeStatus, PlaylistID, SetVideoID, Thumbnail, VideoID, YoutubeID,
};
use ytmapi_rs::error::ErrorKind;
use ytmapi_rs::parse::{ParsedSongArtist, PlaylistItem, SearchResultPlaylist};
use ytmapi_rs::query::{GetLibrarySongsQuery, GetPlaylistTracksQuery};
use ytmapi_rs::YtMusic;

/// `Auth` is HTTP 401/403 or an account call made while anonymous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Auth(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Auth(m) | Self::Other(m)) = self;
        f.write_str(m)
    }
}

impl std::error::Error for ApiError {}

impl From<ytmapi_rs::Error> for ApiError {
    fn from(e: ytmapi_rs::Error) -> Self {
        classify(e).0
    }
}

/// The error plus whether it was a response-shape (parse) failure.
fn classify(e: ytmapi_rs::Error) -> (ApiError, bool) {
    let text = e.to_string();
    match e.into_kind() {
        ErrorKind::OtherErrorCodeInResponse {
            code: 401 | 403, ..
        } => (ApiError::Auth(text), false),
        ErrorKind::JsonParsing(_) => (ApiError::Other(text), true),
        _ => (ApiError::Other(text), false),
    }
}

#[derive(Clone)]
enum Yt {
    Anon(YtMusic<NoAuthToken>),
    Auth(YtMusic<BrowserToken>),
    /// Even the anonymous handshake failed (offline). `reload_auth` retries.
    Down(String),
}

#[derive(Clone)]
pub struct Api {
    yt: Yt,
}

/// Runs `$body` against whichever handle exists (both arms are the same generic code).
macro_rules! any {
    ($self:expr, $yt:ident => $body:expr) => {
        match &$self.yt {
            Yt::Anon($yt) => $body,
            Yt::Auth($yt) => $body,
            Yt::Down(m) => return Err(ApiError::Other(m.clone())),
        }
    };
}

impl Api {
    pub async fn new() -> Self {
        Self {
            yt: connect().await,
        }
    }

    /// Call after a cookie import (or to retry after being offline).
    pub async fn reload_auth(&mut self) {
        self.yt = connect().await;
    }

    /// Not even anonymous: `reload_auth` to retry.
    pub fn is_down(&self) -> bool {
        matches!(self.yt, Yt::Down(_))
    }

    pub fn is_logged_in(&self) -> bool {
        matches!(self.yt, Yt::Auth(_))
    }

    fn auth(&self) -> Result<&YtMusic<BrowserToken>, ApiError> {
        match &self.yt {
            Yt::Auth(yt) => Ok(yt),
            Yt::Anon(_) => Err(ApiError::Auth("log in to use your library".into())),
            Yt::Down(m) => Err(ApiError::Other(m.clone())),
        }
    }

    pub async fn fetch(&self, req: &PageRequest) -> Result<Page, ApiError> {
        match req {
            PageRequest::Search(_)
            | PageRequest::Playlist(_)
            | PageRequest::Album(_)
            | PageRequest::Artist(_) => any!(self, yt => public(yt, req).await),
            PageRequest::LikedSongs => {
                let page = public(self.auth()?, &PageRequest::Playlist("LM".into())).await?;
                Ok(Page {
                    title: "Liked songs".into(),
                    ..page
                })
            }
            _ => library(self.auth()?, req).await,
        }
    }

    /// Up next for `video_id`, without the seed track itself.
    pub async fn radio(&self, video_id: &str) -> Result<Vec<Track>, ApiError> {
        let tracks = any!(self, yt => yt
            .get_watch_playlist_from_video_id(VideoID::from_raw(video_id.to_owned()))
            .await?);
        Ok(tracks
            .iter()
            .filter(|t| t.video_id.get_raw() != video_id)
            .filter_map(|t| {
                track(
                    t.video_id.get_raw(),
                    &t.title,
                    t.author.clone(),
                    None,
                    Some(&t.duration),
                    &t.thumbnails,
                )
            })
            .collect())
    }

    pub async fn rate(&self, video_id: &str, rating: Rating) -> Result<(), ApiError> {
        let status = match rating {
            Rating::Like => LikeStatus::Liked,
            Rating::Dislike => LikeStatus::Disliked,
            Rating::None => LikeStatus::Indifferent,
        };
        Ok(self
            .auth()?
            .rate_song(VideoID::from_raw(video_id.to_owned()), status)
            .await?)
    }

    pub async fn add_to_playlist(&self, playlist_id: &str, video_id: &str) -> Result<(), ApiError> {
        self.auth()?
            .add_video_items_to_playlist(
                PlaylistID::from_raw(bare_playlist_id(playlist_id).to_owned()),
                [VideoID::from_raw(video_id.to_owned())],
            )
            .await?;
        Ok(())
    }

    pub async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> Result<(), ApiError> {
        let yt = self.auth()?;
        let id = bare_playlist_id(playlist_id);
        // ponytail: the contract has no set-video-id, so every removal re-fetches the whole
        // playlist's raw JSON to find it. Carry the id on Track if big playlists make this slow.
        let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw(format!("VL{id}")));
        let pages: Vec<String> = yt.raw_json_stream(&query).try_collect().await?;
        let mut set_ids = Vec::new();
        for page in &pages {
            if let Ok(json) = serde_json::from_str(page) {
                set_video_ids(&json, video_id, &mut set_ids);
            }
        }
        if set_ids.is_empty() {
            return Err(ApiError::Other("track is not in that playlist".into()));
        }
        yt.remove_playlist_items(
            PlaylistID::from_raw(id.to_owned()),
            set_ids.into_iter().map(SetVideoID::from_raw),
        )
        .await?;
        Ok(())
    }

    /// Plain lyrics text, `None` when the track has none.
    pub async fn lyrics(&self, video_id: &str) -> Result<Option<String>, ApiError> {
        let lyrics = any!(self, yt => {
            let id = yt.get_lyrics_id(VideoID::from_raw(video_id.to_owned())).await;
            match id {
                Ok(id) => yt.get_lyrics(id).await,
                Err(e) => Err(e),
            }
        });
        match lyrics {
            Ok(l) if !l.lyrics.trim().is_empty() => Ok(Some(l.lyrics)),
            Ok(_) => Ok(None),
            Err(e) => match classify(e) {
                // No lyrics tab or an unavailable message shows up as a missing JSON path.
                (_, true) => Ok(None),
                (e, false) => Err(e),
            },
        }
    }
}

/// ytmapi-rs has no request timeout: a network that never answers would hang forever.
pub(crate) const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

async fn connect() -> Yt {
    let connect = async {
        if let Some(cookie) = auth::cookie_header() {
            match YtMusic::from_cookie(cookie).await {
                Ok(yt) => return Yt::Auth(yt),
                Err(e) => log::warn!("login cookies unusable, going anonymous: {e}"),
            }
        }
        match YtMusic::new_unauthenticated().await {
            Ok(yt) => Yt::Anon(yt),
            Err(e) => Yt::Down(format!("cannot reach YouTube Music: {e}")),
        }
    };
    tokio::time::timeout(TIMEOUT, connect)
        .await
        .unwrap_or_else(|_| Yt::Down("cannot reach YouTube Music: timed out".into()))
}

/// Pages that work without an account.
async fn public<A: AuthToken>(yt: &YtMusic<A>, req: &PageRequest) -> Result<Page, ApiError> {
    match req {
        PageRequest::Search(q) => {
            let (songs, albums, artists, playlists) = futures::join!(
                yt.search_songs(q.as_str()),
                yt.search_albums(q.as_str()),
                yt.search_artists(q.as_str()),
                yt.search_playlists(q.as_str()),
            );
            let mut items: Vec<Item> = songs?
                .iter()
                .filter_map(|s| {
                    track(
                        s.video_id.get_raw(),
                        &s.title,
                        s.artist.clone(),
                        s.album.as_ref().map(|a| a.name.as_str()),
                        Some(&s.duration),
                        &s.thumbnails,
                    )
                })
                .map(Item::Track)
                .collect();
            // The songs are the point of a search: a failure in the other kinds only drops them.
            match albums {
                Ok(v) => items.extend(v.iter().filter_map(|a| {
                    entry(
                        a.album_id.get_raw(),
                        &a.title,
                        join(&[&a.artist, &a.year]),
                        &a.thumbnails,
                    )
                    .map(Item::Album)
                })),
                Err(e) => log::debug!("search albums failed: {e}"),
            }
            match artists {
                Ok(v) => items.extend(v.iter().filter_map(|a| {
                    entry(
                        a.browse_id.get_raw(),
                        &a.artist,
                        a.subscribers.clone().unwrap_or_default(),
                        &a.thumbnails,
                    )
                    .map(Item::Artist)
                })),
                Err(e) => log::debug!("search artists failed: {e}"),
            }
            match playlists {
                Ok(v) => items.extend(v.iter().filter_map(|p| {
                    let e = match p {
                        SearchResultPlaylist::Featured(p) => entry(
                            bare_playlist_id(p.playlist_id.get_raw()),
                            &p.title,
                            join(&[&p.author, &p.songs]),
                            &p.thumbnails,
                        ),
                        SearchResultPlaylist::Community(p) => entry(
                            bare_playlist_id(p.playlist_id.get_raw()),
                            &p.title,
                            join(&[&p.author, &p.views]),
                            &p.thumbnails,
                        ),
                        SearchResultPlaylist::Podcast(_) => None,
                        _ => None,
                    };
                    e.map(Item::Playlist)
                })),
                Err(e) => log::debug!("search playlists failed: {e}"),
            }
            Ok(Page {
                title: format!("Search: {q}"),
                items,
            })
        }
        PageRequest::Playlist(id) => {
            let browse = PlaylistID::from_raw(format!("VL{}", bare_playlist_id(id)));
            let query = GetPlaylistTracksQuery::new(browse.clone());
            let (details, pages) = futures::join!(
                yt.get_playlist_details(browse),
                yt.stream(&query).try_collect::<Vec<_>>()
            );
            let items = pages?
                .iter()
                .flatten()
                .filter_map(playlist_track)
                .map(Item::Track)
                .collect();
            Ok(Page {
                title: details.map(|d| d.title).unwrap_or_default(),
                items,
            })
        }
        PageRequest::Album(id) => {
            let album = yt.get_album(AlbumID::from_raw(id.clone())).await?;
            let artists = album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let items = album
                .tracks
                .iter()
                .filter_map(|t| {
                    track(
                        t.video_id.get_raw(),
                        &t.title,
                        artists.clone(),
                        Some(&album.title),
                        Some(&t.duration),
                        &album.thumbnails,
                    )
                })
                .map(Item::Track)
                .collect();
            Ok(Page {
                title: album.title,
                items,
            })
        }
        PageRequest::Artist(id) => {
            let artist = yt.get_artist(ArtistChannelID::from_raw(id.clone())).await?;
            let top = &artist.top_releases;
            let mut items: Vec<Item> = top
                .songs
                .iter()
                .flat_map(|s| &s.results)
                .filter_map(|s| {
                    track(
                        s.video_id.get_raw(),
                        &s.title,
                        artist_names(&s.artists),
                        Some(&s.album.name),
                        None,
                        &[],
                    )
                })
                .map(Item::Track)
                .collect();
            items.extend(
                top.albums
                    .iter()
                    .chain(&top.singles)
                    .flat_map(|a| &a.results)
                    .filter_map(|a| {
                        entry(
                            a.album_id.get_raw(),
                            &a.title,
                            a.year.clone(),
                            &a.thumbnails,
                        )
                        .map(Item::Album)
                    }),
            );
            Ok(Page {
                title: artist.name,
                items,
            })
        }
        _ => Err(ApiError::Other("not a public page".into())),
    }
}

/// Pages that need the account.
async fn library(yt: &YtMusic<BrowserToken>, req: &PageRequest) -> Result<Page, ApiError> {
    // ponytail: no paging, the whole library is fetched following continuations to the end.
    // Add a cap or lazy paging when a huge library makes this slow.
    let (title, items) = match req {
        PageRequest::LibraryPlaylists => (
            "Playlists",
            yt.get_library_playlists()
                .await?
                .iter()
                .filter_map(|p| {
                    entry(
                        bare_playlist_id(p.playlist_id.get_raw()),
                        &p.title,
                        join(&[&p.author, &p.tracks]),
                        &p.thumbnails,
                    )
                    .map(Item::Playlist)
                })
                .collect(),
        ),
        PageRequest::LibrarySongs => {
            let query = GetLibrarySongsQuery::default();
            let pages = yt.stream(&query).try_collect::<Vec<_>>().await?;
            (
                "Songs",
                pages
                    .iter()
                    .flatten()
                    .filter(|s| s.is_available)
                    .filter_map(|s| {
                        track(
                            s.video_id.get_raw(),
                            &s.title,
                            artist_names(&s.artists),
                            Some(&s.album.name),
                            Some(&s.duration),
                            &s.thumbnails,
                        )
                    })
                    .map(Item::Track)
                    .collect(),
            )
        }
        PageRequest::LibraryAlbums => (
            "Albums",
            yt.get_library_albums()
                .await?
                .iter()
                .filter_map(|a| {
                    entry(
                        a.album_id.get_raw(),
                        &a.title,
                        join(&[&a.artist, &a.year]),
                        &a.thumbnails,
                    )
                    .map(Item::Album)
                })
                .collect(),
        ),
        PageRequest::LibraryArtists => (
            "Artists",
            yt.get_library_artists()
                .await?
                .iter()
                .filter_map(|a| {
                    entry(a.channel_id.get_raw(), &a.artist, a.byline.clone(), &[])
                        .map(Item::Artist)
                })
                .collect(),
        ),
        _ => return Err(ApiError::Other("not a library page".into())),
    };
    Ok(Page {
        title: title.into(),
        items,
    })
}

fn playlist_track(item: &PlaylistItem) -> Option<Track> {
    match item {
        PlaylistItem::Song(s) if s.is_available => track(
            s.video_id.get_raw(),
            &s.title,
            artist_names(&s.artists),
            Some(&s.album.name),
            Some(&s.duration),
            &s.thumbnails,
        ),
        PlaylistItem::Video(v) if v.is_available => track(
            v.video_id.get_raw(),
            &v.title,
            v.channel_name.clone(),
            None,
            Some(&v.duration),
            &v.thumbnails,
        ),
        _ => {
            log::debug!("skipping playlist item that is unavailable or not a song");
            None
        }
    }
}

fn artist_names(artists: &[ParsedSongArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Joins the non-empty parts with ", ".
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}

/// Playlist ids come back with a `VL` browse prefix in some places and bare in others.
fn bare_playlist_id(id: &str) -> &str {
    id.strip_prefix("VL").unwrap_or(id)
}

/// `None` (logged) when there is nothing playable to show.
fn track(
    video_id: &str,
    title: &str,
    artists: String,
    album: Option<&str>,
    duration: Option<&str>,
    thumbs: &[Thumbnail],
) -> Option<Track> {
    if video_id.is_empty() || title.is_empty() {
        log::debug!("skipping track without video id or title: {video_id:?} {title:?}");
        return None;
    }
    Some(Track {
        video_id: video_id.into(),
        title: title.into(),
        artists,
        album: album.filter(|a| !a.is_empty()).map(Into::into),
        duration_secs: duration.and_then(parse_duration),
        thumbnail_url: thumbnail(thumbs),
    })
}

fn entry(id: &str, title: &str, subtitle: String, thumbs: &[Thumbnail]) -> Option<Entry> {
    if id.is_empty() || title.is_empty() {
        log::debug!("skipping entry without id or title: {id:?} {title:?}");
        return None;
    }
    Some(Entry {
        id: id.into(),
        title: title.into(),
        subtitle,
        thumbnail_url: thumbnail(thumbs),
    })
}

/// "3:45" or "1:02:03" to seconds.
fn parse_duration(s: &str) -> Option<u32> {
    s.trim().split(':').try_fold(0u32, |acc, part| {
        Some(acc.checked_mul(60)? + part.parse::<u32>().ok()?)
    })
}

/// Smallest thumbnail at least 120 px wide (list rows are small), else the largest there is.
fn thumbnail(thumbs: &[Thumbnail]) -> Option<String> {
    thumbs
        .iter()
        .filter(|t| t.width >= 120)
        .min_by_key(|t| t.width)
        .or_else(|| thumbs.iter().max_by_key(|t| t.width))
        .map(|t| t.url.clone())
}

/// Collects `playlistSetVideoId`s of the entries for `video_id`, wherever they sit in the JSON.
fn set_video_ids(json: &serde_json::Value, video_id: &str, out: &mut Vec<String>) {
    match json {
        serde_json::Value::Object(map) => {
            if let Some(data) = map.get("playlistItemData") {
                let field = |k: &str| data.get(k).and_then(|v| v.as_str());
                if let (Some(v), Some(set)) = (field("videoId"), field("playlistSetVideoId")) {
                    if v == video_id {
                        out.push(set.to_owned());
                    }
                }
            }
            map.values().for_each(|v| set_video_ids(v, video_id, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| set_video_ids(v, video_id, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(width: u64) -> Thumbnail {
        serde_json::from_value(serde_json::json!({
            "height": width, "width": width, "url": format!("u{width}")
        }))
        .unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("0:07"), Some(7));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("live"), None);
    }

    #[test]
    fn thumbnail_choice() {
        let t = [thumb(60), thumb(544), thumb(226), thumb(120)];
        assert_eq!(thumbnail(&t).as_deref(), Some("u120"));
        assert_eq!(thumbnail(&[thumb(60), thumb(90)]).as_deref(), Some("u90"));
        assert_eq!(thumbnail(&[]), None);
    }

    #[test]
    fn unmappable_items_are_skipped() {
        assert!(track("", "t", String::new(), None, None, &[]).is_none());
        assert!(track("id", "", String::new(), None, None, &[]).is_none());
        let t = track("id", "t", "a, b".into(), Some(""), Some("1:00"), &[]).unwrap();
        assert_eq!((t.album, t.duration_secs), (None, Some(60)));
        assert!(entry("", "x", String::new(), &[]).is_none());
    }

    #[test]
    fn playlist_ids_and_joins() {
        assert_eq!(bare_playlist_id("VLPL123"), "PL123");
        assert_eq!(bare_playlist_id("LM"), "LM");
        assert_eq!(join(&["Daft Punk", "", "2001"]), "Daft Punk, 2001");
    }

    #[test]
    fn finds_set_video_ids() {
        let json = serde_json::json!({"a": [
            {"playlistItemData": {"videoId": "x", "playlistSetVideoId": "S1"}},
            {"b": {"playlistItemData": {"videoId": "y", "playlistSetVideoId": "S2"}}},
            {"playlistItemData": {"videoId": "x", "playlistSetVideoId": "S3"}},
            {"playlistItemData": {"videoId": "x"}},
        ]});
        let mut out = Vec::new();
        set_video_ids(&json, "x", &mut out);
        assert_eq!(out, ["S1", "S3"]);
    }

    #[test]
    fn anonymous_account_calls_are_auth_errors() {
        let api = Api {
            yt: Yt::Down("offline".into()),
        };
        assert_eq!(api.auth().err(), Some(ApiError::Other("offline".into())));
    }

    // Network tests, anonymous: `cargo test -p cmusic-core -- --ignored api`.
    async fn anon() -> Api {
        Api {
            yt: Yt::Anon(YtMusic::new_unauthenticated().await.unwrap()),
        }
    }

    #[tokio::test]
    #[ignore]
    async fn net_search_tracks_first() {
        let page = anon()
            .await
            .fetch(&PageRequest::Search("daft punk".into()))
            .await
            .unwrap();
        let first = page.items.first().expect("results");
        let Item::Track(t) = first else {
            panic!("tracks come first: {first:?}")
        };
        assert!(!t.video_id.is_empty() && !t.title.is_empty());
        assert!(page.items.iter().any(|i| matches!(i, Item::Album(_))));
        assert!(page.items.iter().any(|i| matches!(i, Item::Artist(_))));
    }

    #[tokio::test]
    #[ignore]
    async fn net_radio() {
        let tracks = anon().await.radio("dQw4w9WgXcQ").await.unwrap();
        assert!(!tracks.is_empty());
        assert!(tracks.iter().all(|t| t.video_id != "dQw4w9WgXcQ"));
    }

    #[tokio::test]
    #[ignore]
    async fn net_album_artist_playlist_pages() {
        let api = anon().await;
        let page = api
            .fetch(&PageRequest::Search("daft punk discovery".into()))
            .await
            .unwrap();
        let id = |f: fn(&Item) -> Option<&Entry>| page.items.iter().find_map(f).unwrap().id.clone();
        let album = id(|i| {
            if let Item::Album(e) = i {
                Some(e)
            } else {
                None
            }
        });
        let artist = id(|i| {
            if let Item::Artist(e) = i {
                Some(e)
            } else {
                None
            }
        });
        let playlist = id(|i| {
            if let Item::Playlist(e) = i {
                Some(e)
            } else {
                None
            }
        });

        let p = api.fetch(&PageRequest::Album(album)).await.unwrap();
        assert!(p.items.iter().any(|i| matches!(i, Item::Track(_))), "{p:?}");
        let p = api.fetch(&PageRequest::Artist(artist)).await.unwrap();
        assert!(p.items.iter().any(|i| matches!(i, Item::Track(_))), "{p:?}");
        let p = api.fetch(&PageRequest::Playlist(playlist)).await.unwrap();
        assert!(p.items.iter().any(|i| matches!(i, Item::Track(_))), "{p:?}");
    }

    #[tokio::test]
    #[ignore]
    async fn net_lyrics_and_library_auth() {
        let api = anon().await;
        // Rick Astley has lyrics on YouTube Music; the call must not error either way.
        let l = api.lyrics("dQw4w9WgXcQ").await.unwrap();
        println!("lyrics: {:?}", l.as_deref().map(|s| &s[..s.len().min(40)]));
        assert!(matches!(
            api.fetch(&PageRequest::LibrarySongs).await,
            Err(ApiError::Auth(_))
        ));
    }

    /// Diagnostic for the stored login: `cargo test -p cmusic-core -- --ignored net_account_pages --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn net_account_pages() {
        let api = Api::new().await;
        println!("logged_in: {}", api.is_logged_in());
        for req in [
            PageRequest::LibraryPlaylists,
            PageRequest::LibraryArtists,
            PageRequest::LibrarySongs,
            PageRequest::LibraryAlbums,
            PageRequest::LikedSongs,
        ] {
            match api.fetch(&req).await {
                Ok(p) => println!("{req:?}: Ok {} items", p.items.len()),
                Err(e) => println!("{req:?}: Err {e}"),
            }
        }
    }
}
