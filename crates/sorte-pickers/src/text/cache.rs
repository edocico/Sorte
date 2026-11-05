//! A bounded cache of shaped lines that evicts one line per insert once full.

use std::collections::{HashMap, VecDeque};

use super::TextLine;

/// Text and font size (its bits).
pub(super) type LineKey = (String, u32);

/// One style's shaped lines, at most `max_lines` of them. Once full, each insert first drops the
/// oldest line, read or not: a frame that shapes new lines (an overlay's, almost every frame)
/// frees as many as it adds, one at a time, and the storage never grows.
pub(super) struct LineCache {
    lines: HashMap<LineKey, TextLine>,
    /// The keys of `lines`, oldest first.
    order: VecDeque<LineKey>,
    max_lines: usize,
}

impl LineCache {
    pub(super) fn new(max_lines: usize) -> Self {
        Self {
            // Room for twice the lines: removals leave tombstones, and once they take the last
            // free slots of a table over half full, hashbrown doubles it (measured 0.4-0.9 ms at
            // 4096 lines). Under half full it cleans them in place instead, rarely: every
            // ~100,000 inserts at 64 lines (3 µs), ~3 million at 4096 (0.2 ms).
            lines: HashMap::with_capacity(2 * max_lines),
            order: VecDeque::with_capacity(max_lines),
            max_lines,
        }
    }

    pub(super) fn get(&self, key: &LineKey) -> Option<TextLine> {
        self.lines.get(key).cloned()
    }

    pub(super) fn len(&self) -> usize {
        self.lines.len()
    }

    /// Caches `line` under `key`, which `get` just missed.
    pub(super) fn insert(&mut self, key: LineKey, line: TextLine) {
        if self.lines.contains_key(&key) {
            return;
        }
        if self.lines.len() >= self.max_lines
            && let Some(oldest) = self.order.pop_front()
        {
            self.lines.remove(&oldest);
        }
        if self.max_lines > 0 {
            self.order.push_back(key.clone());
            self.lines.insert(key, line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: usize) -> LineKey {
        (i.to_string(), 0)
    }

    #[test]
    fn drops_its_oldest_line_once_full() {
        let mut cache = LineCache::new(3);
        for i in 0..3 {
            cache.insert(key(i), TextLine::default());
        }
        // The oldest line goes first, read or not.
        assert!(cache.get(&key(0)).is_some());
        cache.insert(key(3), TextLine::default());
        assert!(cache.get(&key(0)).is_none());
        assert!((1..4).all(|i| cache.get(&key(i)).is_some()));
    }

    #[test]
    fn inserting_a_cached_key_again_changes_nothing() {
        let mut cache = LineCache::new(2);
        cache.insert(key(0), TextLine::default());
        cache.insert(key(0), TextLine { width: 1.0, ..TextLine::default() });
        cache.insert(key(1), TextLine::default());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.order.len(), 2);
        assert_eq!(cache.get(&key(0)).map(|line| line.width), Some(0.0));
    }

    #[test]
    fn a_cache_with_no_room_stays_empty() {
        let mut cache = LineCache::new(0);
        cache.insert(key(0), TextLine::default());
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&key(0)).is_none());
    }

    #[test]
    fn never_grows_under_churn() {
        const LINES: usize = 64;
        let mut cache = LineCache::new(LINES);
        let (map_room, queue_room) = (cache.lines.capacity(), cache.order.capacity());
        for i in 0..100 * LINES {
            cache.insert(key(i), TextLine::default());
            assert!(cache.lines.len() <= LINES && cache.order.len() == cache.lines.len());
            // Neither reallocates. A map's capacity() is its lines plus the free slots that
            // tombstones have not taken, so it rises past its start only if the table grows.
            assert!(cache.lines.capacity() <= map_room && cache.order.capacity() == queue_room);
        }
    }
}
