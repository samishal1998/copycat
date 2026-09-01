//! The append-only hot history and the logical views built over it.
//!
//! Two ideas live here and must not be confused (ADR-002):
//!
//! * the **log** is what actually happened — every external copy, duplicates
//!   included, in order;
//! * a **view** is how a user wants to address that log right now — usually
//!   with accidental repeats collapsed.
//!
//! Collapsing is a property of the view. It never mutates the log.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::clip::{ClipEvent, ClipId, ClipPayload, ClipSource, ClipSummary, ContentHash};

/// Whether a view keeps consecutive repeats or folds them together (R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DuplicatePolicy {
    /// Fold a run of identical adjacent copies into one entry. The default:
    /// the repeats it removes are almost always a slipped keystroke.
    #[default]
    Collapse,
    /// Keep every copy. For deliberately copying the same value twice to paste
    /// it twice.
    Preserve,
}

/// One entry of a logical view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewEntry {
    pub id: ClipId,
    /// Number of consecutive raw events folded into this entry (1 under
    /// `Preserve`, and 1 for an unrepeated copy under `Collapse`).
    pub run: usize,
}

/// Bounded in-memory history. Older events live in the store (§4.2, §4.3).
#[derive(Debug)]
pub struct History {
    /// Chronological, oldest at the front.
    events: VecDeque<ClipEvent>,
    capacity: usize,
    next_id: u64,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        History {
            events: VecDeque::new(),
            capacity: capacity.max(1),
            next_id: 1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Record an external copy. Returns the new event's id.
    ///
    /// Eviction is deferred to [`History::evict`] so the caller can name the
    /// events an active session still needs (R10).
    pub fn append(&mut self, payload: ClipPayload, source: ClipSource, at: i64) -> ClipId {
        let id = ClipId(self.next_id);
        self.next_id += 1;
        self.events.push_back(ClipEvent {
            id,
            captured_at: at,
            source,
            content_hash: payload.content_hash(),
            payload,
            pinned: false,
        });
        id
    }

    /// Trim to capacity, oldest first.
    ///
    /// Pinned events and ids an active session still needs are exempt, and
    /// **exempt events do not count toward the capacity**. Counting them would
    /// force the cap to be met by dropping something newer — with two protected
    /// items and a capacity of two, a third copy would evict itself. Hot memory
    /// is therefore bounded by `capacity + pinned + session size`, all of which
    /// the user controls.
    ///
    /// Returns the ids actually evicted, so callers can drop their payloads.
    pub fn evict(&mut self, protected: &BTreeSet<ClipId>) -> Vec<ClipId> {
        let exempt = |event: &ClipEvent| event.pinned || protected.contains(&event.id);
        let mut evictable = self.events.iter().filter(|e| !exempt(e)).count();

        let mut evicted = Vec::new();
        let mut index = 0;
        while evictable > self.capacity && index < self.events.len() {
            if exempt(&self.events[index]) {
                index += 1;
                continue;
            }
            evicted.push(self.events[index].id);
            self.events.remove(index);
            evictable -= 1;
        }
        evicted
    }

    /// Seed hot history from persisted events, oldest first.
    ///
    /// Ids come from the store rather than being reassigned, so anything that
    /// recorded a clip id before a restart still resolves afterwards.
    pub fn restore(&mut self, events: Vec<ClipEvent>) {
        for event in events {
            self.next_id = self.next_id.max(event.id.0 + 1);
            self.events.push_back(event);
        }
        self.events.make_contiguous().sort_by_key(|e| e.id);
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
    }

    pub fn get(&self, id: ClipId) -> Option<&ClipEvent> {
        self.events.iter().find(|e| e.id == id)
    }

    pub fn latest(&self) -> Option<&ClipEvent> {
        self.events.back()
    }

    /// Chronological, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ClipEvent> {
        self.events.iter()
    }

    /// Newest first, which is how every user-facing surface presents history.
    pub fn view(&self, policy: DuplicatePolicy) -> Vec<ViewEntry> {
        let mut chronological: Vec<ViewEntry> = Vec::with_capacity(self.events.len());
        let mut previous_hash: Option<ContentHash> = None;

        for event in &self.events {
            let repeats_previous = previous_hash == Some(event.content_hash);
            previous_hash = Some(event.content_hash);

            if policy == DuplicatePolicy::Collapse
                && repeats_previous
                && let Some(last) = chronological.last_mut()
            {
                // Keep the newest id of the run: "copied 2s ago" beats
                // "copied 30s ago" for the same text.
                last.id = event.id;
                last.run += 1;
                continue;
            }
            chronological.push(ViewEntry { id: event.id, run: 1 });
        }

        chronological.reverse();
        chronological
    }

    /// Resolve a zero-based offset from latest (R2). `0` is always the newest
    /// entry of the selected view.
    pub fn resolve_offset(&self, offset: usize, policy: DuplicatePolicy) -> Option<ClipId> {
        self.view(policy).get(offset).map(|e| e.id)
    }

    /// The newest `n` logical entries, returned **oldest first** — the order a
    /// queue or a group pastes them in (§3.3, §3.4).
    ///
    /// Fewer than `n` available is not an error (R3).
    pub fn last_n(&self, n: usize, policy: DuplicatePolicy) -> Vec<ClipId> {
        let mut ids: Vec<ClipId> = self.view(policy).into_iter().take(n).map(|e| e.id).collect();
        ids.reverse();
        ids
    }

    pub fn delete(&mut self, id: ClipId) -> bool {
        let before = self.events.len();
        self.events.retain(|e| e.id != id);
        self.events.len() != before
    }

    /// Returns the ids removed, so callers can reconcile session state (R9).
    pub fn clear(&mut self, keep_pinned: bool) -> Vec<ClipId> {
        let mut removed = Vec::new();
        self.events.retain(|e| {
            let keep = keep_pinned && e.pinned;
            if !keep {
                removed.push(e.id);
            }
            keep
        });
        removed
    }

    pub fn set_pinned(&mut self, id: ClipId, pinned: bool) -> bool {
        match self.events.iter_mut().find(|e| e.id == id) {
            Some(event) => {
                event.pinned = pinned;
                true
            }
            None => false,
        }
    }

    /// Case-insensitive substring match over text representations, newest
    /// first. Non-text payloads never match — there is nothing to match against
    /// that would not be a lie.
    pub fn search(&self, query: &str, limit: usize, policy: DuplicatePolicy) -> Vec<ClipSummary> {
        let needle = query.to_lowercase();
        self.summaries(policy)
            .into_iter()
            .filter(|summary| {
                self.get(summary.id)
                    .and_then(|e| e.payload.as_text())
                    .is_some_and(|text| text.to_lowercase().contains(&needle))
            })
            .take(limit)
            .collect()
    }

    /// Newest-first summaries of the logical view, carrying run lengths.
    pub fn summaries(&self, policy: DuplicatePolicy) -> Vec<ClipSummary> {
        self.view(policy)
            .into_iter()
            .filter_map(|entry| {
                self.get(entry.id).map(|event| {
                    let mut summary = event.summary(PREVIEW_CHARS);
                    summary.duplicate_run = entry.run;
                    summary
                })
            })
            .collect()
    }
}

pub const PREVIEW_CHARS: usize = 120;

#[cfg(test)]
mod tests {
    use super::*;

    fn history_of(values: &[&str]) -> History {
        let mut history = History::new(100);
        for (index, value) in values.iter().enumerate() {
            history.append(ClipPayload::text(*value), ClipSource::External, index as i64);
        }
        history
    }

    fn view_texts(history: &History, policy: DuplicatePolicy) -> Vec<String> {
        history
            .view(policy)
            .into_iter()
            .map(|entry| history.get(entry.id).unwrap().payload.as_text().unwrap().to_string())
            .collect()
    }

    #[test]
    fn collapse_folds_only_adjacent_repeats() {
        // R1: the PRD's worked example, plus the non-adjacent case the draft
        // left ambiguous — the trailing B must survive.
        let history = history_of(&["A", "A", "A", "B", "B", "C", "B"]);
        assert_eq!(view_texts(&history, DuplicatePolicy::Collapse), ["B", "C", "B", "A"]);
    }

    #[test]
    fn preserve_keeps_every_copy() {
        let history = history_of(&["A", "A", "A", "B", "B", "C", "B"]);
        assert_eq!(
            view_texts(&history, DuplicatePolicy::Preserve),
            ["B", "C", "B", "B", "A", "A", "A"]
        );
    }

    #[test]
    fn collapsed_entry_carries_the_run_length_and_the_newest_id() {
        let history = history_of(&["A", "A", "A", "B"]);
        let view = history.view(DuplicatePolicy::Collapse);
        assert_eq!(view[1].run, 3);
        assert_eq!(view[1].id, ClipId(3), "run should be addressed by its newest event");
        assert_eq!(view[0].run, 1);
    }

    #[test]
    fn offsets_index_the_collapsed_view_by_default() {
        // R2: after a double-tapped copy, offset 1 must not be the same text.
        let history = history_of(&["A", "B", "B"]);
        let collapsed = history.resolve_offset(1, DuplicatePolicy::Collapse).unwrap();
        assert_eq!(history.get(collapsed).unwrap().payload.as_text(), Some("A"));

        let raw = history.resolve_offset(1, DuplicatePolicy::Preserve).unwrap();
        assert_eq!(history.get(raw).unwrap().payload.as_text(), Some("B"));
    }

    #[test]
    fn offset_past_the_end_resolves_to_nothing() {
        let history = history_of(&["A"]);
        assert_eq!(history.resolve_offset(1, DuplicatePolicy::Collapse), None);
    }

    #[test]
    fn last_n_is_oldest_first_and_takes_what_exists() {
        // R3: asking for five when three exist yields three, not an error.
        let history = history_of(&["A", "B", "C"]);
        let ids = history.last_n(5, DuplicatePolicy::Collapse);
        let texts: Vec<_> = ids
            .iter()
            .map(|id| history.get(*id).unwrap().payload.as_text().unwrap())
            .collect();
        assert_eq!(texts, ["A", "B", "C"]);
    }

    #[test]
    fn eviction_takes_the_oldest_unexempt_event() {
        let mut history = History::new(2);
        for value in ["A", "B", "C", "D", "E"] {
            history.append(ClipPayload::text(value), ClipSource::External, 0);
        }
        history.set_pinned(ClipId(1), true);
        let protected = BTreeSet::from([ClipId(2)]);

        let evicted = history.evict(&protected);

        assert_eq!(evicted, vec![ClipId(3)]);
        assert!(history.get(ClipId(1)).is_some(), "pinned survives");
        assert!(history.get(ClipId(2)).is_some(), "protected survives");
        assert!(history.get(ClipId(5)).is_some(), "the newest is never evicted");
        assert_eq!(history.len(), 4);
    }

    #[test]
    fn a_protected_event_never_evicts_something_newer_than_itself() {
        // The whole reason exempt events are excluded from the count: a hard
        // cap here would have to evict C, the item just copied.
        let mut history = History::new(2);
        for value in ["A", "B", "C"] {
            history.append(ClipPayload::text(value), ClipSource::External, 0);
        }
        let protected = BTreeSet::from([ClipId(1), ClipId(2)]);

        assert!(history.evict(&protected).is_empty());
        assert!(history.get(ClipId(3)).is_some());
    }

    #[test]
    fn clear_can_keep_pinned_items() {
        let mut history = history_of(&["A", "B", "C"]);
        history.set_pinned(ClipId(2), true);
        let removed = history.clear(true);
        assert_eq!(removed, vec![ClipId(1), ClipId(3)]);
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn search_is_case_insensitive_and_newest_first() {
        let history = history_of(&["Postgres URL", "redis", "postgres pool"]);
        let hits = history.search("POSTGRES", 10, DuplicatePolicy::Collapse);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].preview, "postgres pool");
    }

    #[test]
    fn search_skips_payloads_with_no_text() {
        use crate::clip::Representation;
        let mut history = History::new(10);
        history.append(
            ClipPayload {
                representations: vec![Representation {
                    media_type: "image/png".into(),
                    bytes: b"needle".to_vec(),
                }],
            },
            ClipSource::External,
            0,
        );
        assert!(history.search("needle", 10, DuplicatePolicy::Collapse).is_empty());
    }
}
