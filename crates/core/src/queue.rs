//! The play queue: tracks, cursor, shuffle and repeat. Pure data, no IO.

use std::collections::HashMap;

use crate::types::{Repeat, Track};

/// The track list in play order, plus the order it was added in (restored when shuffle goes off).
/// Indexes taken and returned by every method are play-order indexes, as in `Snapshot::queue`.
#[derive(Debug, Clone)]
pub struct Queue {
    /// Play order. The `u64` is a unique entry id, so two copies of one video stay distinct.
    entries: Vec<(u64, Track)>,
    /// Entry ids in insertion order.
    original: Vec<u64>,
    /// Index into `entries`, always in range when `Some`.
    cursor: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
    last_id: u64,
    rng: u64,
}

impl Queue {
    /// Empty queue. `seed` drives shuffling.
    pub fn new(seed: u64) -> Self {
        Self {
            entries: Vec::new(),
            original: Vec::new(),
            cursor: None,
            shuffle: false,
            repeat: Repeat::Off,
            last_id: 0,
            rng: seed | 1, // xorshift state must never be zero
        }
    }

    /// Tracks in play order.
    pub fn tracks(&self) -> Vec<Track> {
        self.entries.iter().map(|(_, t)| t.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Index of the current track.
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn get(&self, index: usize) -> Option<&Track> {
        self.entries.get(index).map(|(_, t)| t)
    }

    pub fn current(&self) -> Option<&Track> {
        self.cursor.and_then(|i| self.get(i))
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.entries.iter().any(|(_, t)| t.video_id == video_id)
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Tracks after the current one (all of them when there is no current track).
    pub fn remaining(&self) -> usize {
        self.cursor
            .map_or(self.len(), |c| self.len().saturating_sub(c + 1))
    }

    /// Replace everything; `start` (clamped) becomes current. Reshuffles when shuffle is on.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) {
        self.entries = self.wrap(tracks);
        self.original = self.entries.iter().map(|(id, _)| *id).collect();
        self.cursor = self.len().checked_sub(1).map(|last| start.min(last));
        if self.shuffle {
            self.reshuffle();
        }
    }

    /// Load a saved queue as is, without reshuffling.
    pub fn restore(
        &mut self,
        tracks: Vec<Track>,
        cursor: Option<usize>,
        shuffle: bool,
        repeat: Repeat,
        ranks: &[usize],
    ) {
        self.shuffle = false;
        self.replace(tracks, 0);
        // `ranks[i]` is the insertion-order position of play-order entry `i`; anything that is
        // not a permutation (old state file, hand edit) keeps the play order as original.
        let mut seen = vec![false; self.len()];
        let valid = ranks.len() == self.len()
            && ranks
                .iter()
                .all(|&r| seen.get_mut(r).is_some_and(|s| !std::mem::replace(s, true)));
        if valid {
            for (&r, (id, _)) in ranks.iter().zip(&self.entries) {
                self.original[r] = *id;
            }
        }
        self.cursor = cursor.filter(|&c| c < self.len());
        self.shuffle = shuffle;
        self.repeat = repeat;
    }

    /// For each play-order entry, its position in insertion order (what `restore` takes back).
    pub fn original_ranks(&self) -> Vec<usize> {
        let rank: HashMap<u64, usize> = self
            .original
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        self.entries.iter().map(|(id, _)| rank[id]).collect()
    }

    /// Add at the end.
    pub fn append(&mut self, tracks: Vec<Track>) {
        let new = self.wrap(tracks);
        self.original.extend(new.iter().map(|(id, _)| *id));
        self.entries.extend(new);
    }

    /// Insert right after the current track (at the end when there is none).
    pub fn insert_next(&mut self, tracks: Vec<Track>) {
        let Some((c, cur_id)) = self.cursor.and_then(|c| Some((c, self.entries.get(c)?.0))) else {
            return self.append(tracks);
        };
        let new = self.wrap(tracks);
        let at = self
            .original
            .iter()
            .position(|&id| id == cur_id)
            .map_or(self.original.len(), |p| p + 1);
        self.original.splice(at..at, new.iter().map(|(id, _)| *id));
        self.entries.splice(c + 1..c + 1, new);
    }

    /// Remove the track at `index`. Removing the current track makes the following one current;
    /// when it was the last, the cursor wraps to the first with `Repeat::All` and otherwise
    /// moves to the new last track (none only when the queue is empty). Returns false when
    /// `index` is out of range.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.len() {
            return false;
        }
        let (id, _) = self.entries.remove(index);
        self.original.retain(|&o| o != id);
        self.cursor = match self.cursor {
            Some(c) if index < c => Some(c - 1),
            Some(c) if index == c && c >= self.len() => {
                let last = self.len().checked_sub(1);
                if self.repeat == Repeat::All {
                    last.map(|_| 0)
                } else {
                    last
                }
            }
            other => other,
        };
        true
    }

    /// Make `index` current. Returns false when out of range.
    pub fn jump(&mut self, index: usize) -> bool {
        let ok = index < self.len();
        if ok {
            self.cursor = Some(index);
        }
        ok
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.original.clear();
        self.cursor = None;
    }

    /// Shuffle on: the current track moves to the front, the rest is shuffled after it.
    /// Shuffle off: insertion order comes back. The current track stays current either way.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if self.shuffle {
            return self.reshuffle();
        }
        let cur = self.cursor.and_then(|c| self.entries.get(c)).map(|e| e.0);
        let rank: HashMap<u64, usize> = self
            .original
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        self.entries.sort_by_key(|(id, _)| rank.get(id).copied());
        self.cursor = cur.and_then(|id| self.entries.iter().position(|e| e.0 == id));
    }

    /// Index of the track after the current one. `manual` (the Next button) ignores
    /// `Repeat::One`; automatic advance replays the same track.
    pub fn peek_next(&self, manual: bool) -> Option<usize> {
        let c = self.cursor?;
        match self.repeat {
            Repeat::One if !manual => Some(c),
            _ if c + 1 < self.len() => Some(c + 1),
            Repeat::All => Some(0),
            _ => None,
        }
    }

    /// Move to `peek_next(manual)`. Returns false (cursor unchanged) at the end of the queue.
    pub fn advance(&mut self, manual: bool) -> bool {
        let next = self.peek_next(manual);
        if next.is_some() {
            self.cursor = next;
        }
        next.is_some()
    }

    /// Move to the previous track, wrapping only with `Repeat::All`. Returns false if it cannot.
    pub fn go_back(&mut self) -> bool {
        let prev = match self.cursor {
            Some(c) if c > 0 => c - 1,
            Some(_) if self.repeat == Repeat::All => self.len() - 1,
            _ => return false,
        };
        self.cursor = Some(prev);
        true
    }

    fn wrap(&mut self, tracks: Vec<Track>) -> Vec<(u64, Track)> {
        tracks
            .into_iter()
            .map(|t| {
                self.last_id += 1;
                (self.last_id, t)
            })
            .collect()
    }

    fn reshuffle(&mut self) {
        let current = self
            .cursor
            .filter(|&c| c < self.len())
            .map(|c| self.entries.remove(c));
        for i in (1..self.entries.len()).rev() {
            let j = (self.rand() % (i as u64 + 1)) as usize;
            self.entries.swap(i, j);
        }
        if let Some(cur) = current {
            self.entries.insert(0, cur);
            self.cursor = Some(0);
        }
    }

    // ponytail: xorshift64 plus modulo, slightly biased, fine for shuffling a playlist.
    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(ids: &str, cursor: usize) -> Queue {
        let mut q = Queue::new(42);
        let tracks = ids
            .split_whitespace()
            .map(|id| Track {
                video_id: id.into(),
                title: id.into(),
                artists: String::new(),
                album: None,
                duration_secs: None,
                thumbnail_url: None,
            })
            .collect();
        q.replace(tracks, cursor);
        q
    }

    fn ids(q: &Queue) -> String {
        q.tracks()
            .iter()
            .map(|t| t.video_id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn cur(q: &Queue) -> &str {
        q.current().map_or("-", |t| t.video_id.as_str())
    }

    #[test]
    fn remove_before_at_after_cursor() {
        // (remove index, queue after, current after)
        let cases = [
            (0, "b c d", "c"),
            (2, "a b d", "d"),
            (3, "a b c", "c"),
            (9, "a b c d", "c"),
        ];
        for (index, want_ids, want_cur) in cases {
            let mut q = q("a b c d", 2);
            q.remove(index);
            assert_eq!(
                (ids(&q).as_str(), cur(&q)),
                (want_ids, want_cur),
                "remove {index}"
            );
        }
        let mut last = q("a b", 1);
        last.remove(1);
        assert_eq!(last.cursor(), Some(0));
        let mut wrap = q("a b c", 2);
        wrap.set_repeat(Repeat::All);
        wrap.remove(2);
        assert_eq!(wrap.cursor(), Some(0));
        let mut one = q("a", 0);
        one.remove(0);
        assert_eq!(one.cursor(), None);
    }

    #[test]
    fn peek_next_honors_repeat() {
        // (cursor, repeat, manual, want)
        let cases = [
            (0, Repeat::Off, false, Some(1)),
            (2, Repeat::Off, false, None),
            (2, Repeat::All, false, Some(0)),
            (1, Repeat::One, false, Some(1)),
            (1, Repeat::One, true, Some(2)),
            (2, Repeat::One, true, None),
        ];
        for (cursor, repeat, manual, want) in cases {
            let mut q = q("a b c", cursor);
            q.set_repeat(repeat);
            assert_eq!(q.peek_next(manual), want, "{cursor} {repeat:?} {manual}");
        }
        assert_eq!(Queue::new(1).peek_next(false), None);
    }

    #[test]
    fn shuffle_keeps_current_and_restores_order() {
        let mut q = q("a b c d e f g h", 3);
        q.toggle_shuffle();
        assert_eq!(cur(&q), "d");
        assert_eq!(q.cursor(), Some(0));
        assert_ne!(ids(&q), "d a b c e f g h", "seed 42 should move something");
        let mut sorted: Vec<_> = q.tracks().into_iter().map(|t| t.video_id).collect();
        sorted.sort();
        assert_eq!(sorted.join(" "), "a b c d e f g h");
        q.toggle_shuffle();
        assert_eq!((ids(&q).as_str(), cur(&q)), ("a b c d e f g h", "d"));
    }

    #[test]
    fn restore_brings_back_the_original_order() {
        let mut a = q("a b c d e f", 2);
        a.toggle_shuffle();
        let (shuffled, ranks) = (a.tracks(), a.original_ranks());
        let mut b = Queue::new(9);
        b.restore(shuffled, Some(0), true, Repeat::Off, &ranks);
        b.toggle_shuffle();
        assert_eq!((ids(&b).as_str(), cur(&b)), ("a b c d e f", "c"));
        // Old state files (no ranks) and nonsense ranks keep the saved order.
        for bad in [&[][..], &[0, 0, 1, 2, 3, 4], &[9, 1, 2, 3, 4, 5]] {
            let mut c = Queue::new(9);
            c.restore(a.tracks(), Some(0), true, Repeat::Off, bad);
            c.toggle_shuffle();
            assert_eq!(ids(&c), ids(&a));
        }
    }

    #[test]
    fn edits_while_shuffled_land_in_original_order() {
        let mut q = q("a b c d", 1);
        q.toggle_shuffle();
        q.insert_next(vec![q.get(0).unwrap().clone()]); // a second "b" right after b
        q.append(q.tracks()[..1].to_vec());
        q.remove(5); // the appended copy, last in play order
        q.toggle_shuffle();
        assert_eq!((ids(&q).as_str(), cur(&q)), ("a b b c d", "b"));
        assert_eq!(q.cursor(), Some(1));
    }

    #[test]
    fn go_back_and_insert_next() {
        let mut q = q("a b c", 0);
        assert!(!q.go_back());
        q.set_repeat(Repeat::All);
        assert!(q.go_back());
        assert_eq!(cur(&q), "c");
        let mut empty = Queue::new(0);
        empty.insert_next(Vec::new());
        assert!(!empty.go_back() && !empty.advance(true) && !empty.remove(0) && !empty.jump(0));
        empty.toggle_shuffle();
        empty.toggle_shuffle();
    }
}
