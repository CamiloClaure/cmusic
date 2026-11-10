//! Synced lyrics from LRCLIB, cached on disk by video id.

use crate::{persist::cache_dir, Lyrics, Track};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Parses `[mm:ss.xx] text` lines. Several timestamps per line repeat the text, metadata tags
/// like `[ar:..]` are skipped, the result is sorted by time.
pub fn parse_lrc(text: &str) -> Lyrics {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(inner) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            let (tag, after) = inner;
            // not a timestamp (metadata tag like [ar:..]): stop, the line yields nothing
            let Some(t) = parse_stamp(tag) else { break };
            stamps.push(t);
            rest = after;
        }
        let text = rest.trim();
        for t in stamps {
            lines.push((Some(t), text.to_string()));
        }
    }
    lines.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Lyrics { lines }
}

fn parse_stamp(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let m: f64 = m
        .parse()
        .ok()
        .filter(|_| m.bytes().all(|b| b.is_ascii_digit()))?;
    if !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    Some(m * 60.0 + s.parse::<f64>().ok()?)
}

pub fn plain(text: &str) -> Lyrics {
    Lyrics {
        lines: text
            .lines()
            .map(|l| (None, l.trim_end().to_string()))
            .collect(),
    }
}

/// Index of the active synced line: the last one that started at or before `position_secs`.
pub fn current_line(lyrics: &Lyrics, position_secs: f64) -> Option<usize> {
    let n = lyrics
        .lines
        .iter()
        .take_while(|(t, _)| t.is_some_and(|t| t <= position_secs))
        .count();
    n.checked_sub(1)
}

#[derive(Serialize, Deserialize, Default)]
struct Raw {
    #[serde(default, rename = "syncedLyrics")]
    synced: Option<String>,
    #[serde(default, rename = "plainLyrics")]
    plain: Option<String>,
}

impl Raw {
    fn lyrics(&self) -> Option<Lyrics> {
        let nonempty = |s: &&String| !s.trim().is_empty();
        if let Some(s) = self.synced.as_ref().filter(nonempty) {
            let l = parse_lrc(s);
            if !l.lines.is_empty() {
                return Some(l);
            }
        }
        self.plain.as_ref().filter(nonempty).map(|p| plain(p))
    }
}

fn cache_path(video_id: &str) -> Option<std::path::PathBuf> {
    let ok = !video_id.is_empty()
        && video_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| cache_dir().join("lyrics").join(format!("{video_id}.json")))
}

/// Cache, then LRCLIB. `None` when nothing is found or on any error.
pub async fn fetch(track: &Track) -> Option<Lyrics> {
    let path = cache_path(&track.video_id);
    if let Some(raw) = path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Raw>(&b).ok())
    {
        return raw.lyrics();
    }
    let raw = match lrclib(track).await {
        Ok(r) => r,
        Err(e) => {
            // Network trouble is not "no lyrics": do not cache it.
            log::warn!("lrclib failed for {}: {e}", track.video_id);
            return None;
        }
    };
    if let Some(p) = path {
        // ponytail: a cached "not found" never expires; delete the cache dir to retry.
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(p.parent().unwrap_or(&p))?;
            std::fs::write(&p, serde_json::to_vec(&raw)?)
        };
        if let Err(e) = write() {
            log::warn!("cannot cache lyrics: {e}");
        }
    }
    raw.lyrics()
}

async fn lrclib(track: &Track) -> Result<Raw, reqwest::Error> {
    let client = reqwest::Client::builder()
        .user_agent("cmusic (https://github.com/CamiloClaure/cmusic)")
        .timeout(Duration::from_secs(10))
        .build()?;
    let artist = track.artists.split(", ").next().unwrap_or("");
    let q = vec![
        ("track_name", track.title.clone()),
        ("artist_name", artist.to_string()),
    ];
    let mut get = q.clone();
    if let Some(a) = &track.album {
        get.push(("album_name", a.clone()));
    }
    if let Some(d) = track.duration_secs {
        get.push(("duration", d.to_string()));
    }
    let res = client
        .get("https://lrclib.net/api/get")
        .query(&get)
        .send()
        .await?;
    if res.status() != reqwest::StatusCode::NOT_FOUND {
        return res.error_for_status()?.json().await;
    }
    let hits: Vec<Raw> = client
        .get("https://lrclib.net/api/search")
        .query(&q)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(hits
        .into_iter()
        .find(|r| r.lyrics().is_some())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc() {
        let l = parse_lrc(
            "[ar:Someone]\n[ti:Song]\n[00:12.50] second\n[00:01.5][00:30.250] chorus\n[00:20.00]\n[01:05.123] end\nno stamp\n",
        );
        let got: Vec<_> = l
            .lines
            .iter()
            .map(|(t, s)| (t.unwrap(), s.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1.5, "chorus"),
                (12.5, "second"),
                (20.0, ""),
                (30.25, "chorus"),
                (65.123, "end")
            ]
        );
    }

    #[test]
    fn plain_lines() {
        assert_eq!(
            plain("a\nb").lines,
            vec![(None, "a".to_string()), (None, "b".to_string())]
        );
    }

    #[test]
    fn current_line_boundaries() {
        let l = parse_lrc("[00:10.00] a\n[00:20.00] b");
        assert_eq!(current_line(&l, 0.0), None);
        assert_eq!(current_line(&l, 9.99), None);
        assert_eq!(current_line(&l, 10.0), Some(0));
        assert_eq!(current_line(&l, 19.9), Some(0));
        assert_eq!(current_line(&l, 20.0), Some(1));
        assert_eq!(current_line(&l, 999.0), Some(1));
        assert_eq!(current_line(&plain("x"), 5.0), None);
    }

    #[test]
    fn bad_video_id_has_no_path() {
        assert!(cache_path("../x").is_none());
        assert!(cache_path("dQw4w9WgXcQ").is_some());
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn fetches_synced_lyrics() {
        let t = Track {
            video_id: "test-rickroll-fetch".into(),
            title: "Never Gonna Give You Up".into(),
            artists: "Rick Astley".into(),
            album: None,
            duration_secs: Some(213),
            thumbnail_url: None,
        };
        if let Some(p) = cache_path(&t.video_id) {
            std::fs::remove_file(p).ok();
        }
        let l = fetch(&t).await.expect("lyrics");
        assert!(l.lines.len() > 10 && l.lines.iter().any(|(t, _)| t.is_some()));
    }
}
