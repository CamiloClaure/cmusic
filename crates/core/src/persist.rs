//! Queue and player settings saved as `state.json`. Never fatal: a bad file just means a fresh start.

use crate::{Repeat, Snapshot, Track};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Saved {
    /// The play-order queue.
    pub queue: Vec<Track>,
    /// Insertion-order position of each queue entry (restores the order when shuffle goes off).
    /// Empty in files from before it was saved.
    pub original: Vec<usize>,
    pub cursor: Option<usize>,
    pub position_secs: f64,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub normalize: bool,
}

impl From<&Snapshot> for Saved {
    fn from(s: &Snapshot) -> Self {
        Saved {
            queue: s.queue.clone(),
            original: Vec::new(),
            cursor: s.cursor,
            position_secs: s.position_secs,
            volume: s.volume,
            shuffle: s.shuffle,
            repeat: s.repeat,
            normalize: s.normalize,
        }
    }
}

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "cmusic")
}

/// `CMUSIC_STATE_DIR`, when set, overrides the directory, so the `play` example (or any manual run)
/// never reads or overwrites the user's saved session.
pub fn state_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CMUSIC_STATE_DIR") {
        return PathBuf::from(dir).join("state.json");
    }
    match dirs() {
        Some(d) => d
            .state_dir()
            .unwrap_or_else(|| d.data_local_dir())
            .join("state.json"),
        None => PathBuf::from("cmusic-state.json"),
    }
}

pub fn cache_dir() -> PathBuf {
    let dir = match dirs() {
        Some(d) => d.cache_dir().to_path_buf(),
        None => std::env::temp_dir().join("cmusic-cache"),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("cannot create cache dir {}: {e}", dir.display());
    }
    dir
}

pub fn load() -> Option<Saved> {
    load_from(&state_path())
}

pub fn save(s: &Saved) -> std::io::Result<()> {
    save_to(&state_path(), s)
}

pub(crate) fn load_from(path: &Path) -> Option<Saved> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(s) => Some(s),
        Err(e) => {
            // Keep it for rescue: the next save would overwrite it with a fresh session.
            let bad = path.with_extension("json.bad");
            log::warn!("corrupt {} moved to {}: {e}", path.display(), bad.display());
            let _ = std::fs::rename(path, bad);
            None
        }
    }
}

pub(crate) fn save_to(path: &Path, s: &Saved) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cmusic-persist-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trip() {
        let dir = tmp("rt");
        let path = dir.join("sub").join("state.json");
        let s = Saved {
            queue: vec![Track {
                video_id: "abc".into(),
                title: "t".into(),
                artists: "a, b".into(),
                album: None,
                duration_secs: Some(10),
                thumbnail_url: None,
            }],
            original: vec![0],
            cursor: Some(0),
            position_secs: 12.5,
            volume: 80,
            shuffle: true,
            repeat: Repeat::All,
            normalize: true,
        };
        save_to(&path, &s).unwrap();
        assert_eq!(load_from(&path), Some(s));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_and_missing_give_none() {
        let dir = tmp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        assert_eq!(load_from(&path), None);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_from(&path), None);
        // Kept as .bad (an older one is replaced), not left to be overwritten.
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(path.with_extension("json.bad")).unwrap(),
            b"{not json"
        );
        std::fs::write(&path, "[2]").unwrap();
        assert_eq!(load_from(&path), None);
        assert_eq!(
            std::fs::read(path.with_extension("json.bad")).unwrap(),
            b"[2]"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn older_file_with_missing_fields_loads() {
        let dir = tmp("old");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"volume":40}"#).unwrap();
        assert_eq!(load_from(&path).unwrap().volume, 40);
        std::fs::remove_dir_all(dir).ok();
    }
}
