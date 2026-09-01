//! Traversal and capture sessions.
//!
//! A session is a cursor over a list of clip ids. Stack, queue, and group
//! differ only in how that list is built and how new copies enter it — which
//! is why they share one type instead of three.
//!
//! Sessions never own payloads and never delete history. Consuming an item
//! moves a cursor (STATE_MACHINE invariant 4).

use serde::{Deserialize, Serialize};

use crate::clip::ClipId;
use crate::history::DuplicatePolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Stack,
    Queue,
    Group,
}

impl SessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::Stack => "stack",
            SessionMode::Queue => "queue",
            SessionMode::Group => "group",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Accepting new copies; not yet traversable.
    Capturing,
    /// Fixed contents; traversable.
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub mode: SessionMode,
    pub duplicate_policy: DuplicatePolicy,
    pub state: SessionState,
    /// In paste order: index 0 is the next item for both stack (newest first)
    /// and queue (oldest first). The orders differ; the traversal does not.
    pub items: Vec<ClipId>,
    pub cursor: usize,
    pub delimiter: Option<String>,
    pub created_at: i64,
}

impl Session {
    pub fn new(
        id: u64,
        mode: SessionMode,
        duplicate_policy: DuplicatePolicy,
        state: SessionState,
        items: Vec<ClipId>,
        delimiter: Option<String>,
        created_at: i64,
    ) -> Self {
        Session {
            id,
            mode,
            duplicate_policy,
            state,
            items,
            cursor: 0,
            delimiter,
            created_at,
        }
    }

    pub fn next_item(&self) -> Option<ClipId> {
        self.items.get(self.cursor).copied()
    }

    pub fn remaining(&self) -> usize {
        self.items.len().saturating_sub(self.cursor)
    }

    pub fn is_exhausted(&self) -> bool {
        self.cursor >= self.items.len()
    }

    pub fn advance(&mut self) {
        self.cursor = (self.cursor + 1).min(self.items.len());
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// Freeze a capture session so it can be traversed (`queue seal`).
    pub fn seal(&mut self) {
        self.state = SessionState::Ready;
        self.cursor = 0;
    }

    /// The item an incoming copy is compared against under `collapse`.
    ///
    /// A stack compares against the item about to be pasted, because that is
    /// what the new copy would sit on top of (R7). A capture compares against
    /// the tail, because that is what was captured most recently (R8).
    pub fn collapse_target(&self) -> Option<ClipId> {
        match self.mode {
            SessionMode::Stack => self.next_item(),
            SessionMode::Queue | SessionMode::Group => self.items.last().copied(),
        }
    }

    /// Whether this session takes in copies that arrive while it is alive.
    pub fn accepts_copies(&self) -> bool {
        match self.mode {
            // A live stack keeps growing at the cursor.
            SessionMode::Stack => true,
            // A queue or group only grows while capturing; a sealed queue is a
            // snapshot and must not drift (STATE_MACHINE invariant 6).
            SessionMode::Queue | SessionMode::Group => self.state == SessionState::Capturing,
        }
    }

    /// Offer a newly copied clip to the session.
    ///
    /// `duplicates_target` says whether the incoming payload matches
    /// [`Session::collapse_target`]; the caller resolves that because only it
    /// can see payloads. Returns whether the clip entered the session.
    pub fn accept(&mut self, id: ClipId, duplicates_target: bool) -> bool {
        if !self.accepts_copies() {
            return false;
        }
        if self.duplicate_policy == DuplicatePolicy::Collapse && duplicates_target {
            return false;
        }
        match self.mode {
            // Insert at the cursor so the new copy is next, without disturbing
            // what has already been consumed (R7).
            SessionMode::Stack => {
                let at = self.cursor.min(self.items.len());
                self.items.insert(at, id);
            }
            SessionMode::Queue | SessionMode::Group => self.items.push(id),
        }
        true
    }

    /// Drop a deleted clip from this session (R9).
    ///
    /// Removing an item the cursor has already passed shifts everything left,
    /// so the cursor moves with it; otherwise the session would silently skip
    /// an item the user never consumed.
    pub fn remove_clip(&mut self, id: ClipId) -> bool {
        let mut removed = false;
        let mut index = 0;
        while index < self.items.len() {
            if self.items[index] == id {
                self.items.remove(index);
                if index < self.cursor {
                    self.cursor -= 1;
                }
                removed = true;
            } else {
                index += 1;
            }
        }
        self.cursor = self.cursor.min(self.items.len());
        removed
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id,
            mode: self.mode,
            state: self.state,
            duplicate_policy: self.duplicate_policy,
            size: self.items.len(),
            cursor: self.cursor,
            remaining: self.remaining(),
            next: self.next_item(),
            delimiter: self.delimiter.clone(),
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: u64,
    pub mode: SessionMode,
    pub state: SessionState,
    pub duplicate_policy: DuplicatePolicy,
    pub size: usize,
    pub cursor: usize,
    pub remaining: usize,
    pub next: Option<ClipId>,
    pub delimiter: Option<String>,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(items: &[u64]) -> Session {
        Session::new(
            1,
            SessionMode::Stack,
            DuplicatePolicy::Collapse,
            SessionState::Ready,
            items.iter().copied().map(ClipId).collect(),
            None,
            0,
        )
    }

    fn capture(mode: SessionMode) -> Session {
        Session::new(1, mode, DuplicatePolicy::Collapse, SessionState::Capturing, vec![], None, 0)
    }

    #[test]
    fn stack_push_lands_at_the_cursor() {
        // R7 and the PRD's worked example: with the cursor on B, copying D
        // makes D next and B the one after.
        let mut session = stack(&[3, 2, 1]);
        session.advance();
        assert_eq!(session.next_item(), Some(ClipId(2)));

        session.accept(ClipId(4), false);

        assert_eq!(session.next_item(), Some(ClipId(4)));
        session.advance();
        assert_eq!(session.next_item(), Some(ClipId(2)));
    }

    #[test]
    fn collapse_drops_a_push_matching_the_item_at_the_cursor() {
        let mut session = stack(&[3, 2, 1]);
        assert!(!session.accept(ClipId(4), true));
        assert_eq!(session.items.len(), 3);
    }

    #[test]
    fn preserve_keeps_a_push_matching_the_item_at_the_cursor() {
        let mut session = stack(&[3, 2, 1]);
        session.duplicate_policy = DuplicatePolicy::Preserve;
        assert!(session.accept(ClipId(4), true));
        assert_eq!(session.next_item(), Some(ClipId(4)));
    }

    #[test]
    fn a_sealed_queue_is_a_snapshot() {
        // STATE_MACHINE invariant 6.
        let mut session = capture(SessionMode::Queue);
        session.accept(ClipId(1), false);
        session.seal();
        assert!(!session.accept(ClipId(2), false));
        assert_eq!(session.items, vec![ClipId(1)]);
    }

    #[test]
    fn capture_compares_against_the_tail_and_a_stack_against_the_cursor() {
        // R8 versus R7 — the two collapse targets are genuinely different.
        let queue = {
            let mut s = capture(SessionMode::Queue);
            s.accept(ClipId(1), false);
            s.accept(ClipId(2), false);
            s
        };
        assert_eq!(queue.collapse_target(), Some(ClipId(2)));

        let mut stack = stack(&[3, 2, 1]);
        stack.advance();
        assert_eq!(stack.collapse_target(), Some(ClipId(2)));
    }

    #[test]
    fn capture_seals_into_fifo_order() {
        let mut session = capture(SessionMode::Queue);
        for id in [1, 2, 3] {
            session.accept(ClipId(id), false);
        }
        session.seal();
        assert_eq!(session.next_item(), Some(ClipId(1)));
    }

    #[test]
    fn advance_stops_at_exhaustion_rather_than_wrapping() {
        // R6: no wrap, ever.
        let mut session = stack(&[1]);
        session.advance();
        assert!(session.is_exhausted());
        session.advance();
        assert_eq!(session.cursor, 1);
        assert_eq!(session.next_item(), None);
    }

    #[test]
    fn deleting_a_consumed_item_moves_the_cursor_with_it() {
        // R9: without the cursor adjustment the session would skip ClipId(1).
        let mut session = stack(&[3, 2, 1]);
        session.advance();
        session.advance();
        assert_eq!(session.next_item(), Some(ClipId(1)));

        session.remove_clip(ClipId(3));

        assert_eq!(session.cursor, 1);
        assert_eq!(session.next_item(), Some(ClipId(1)));
    }

    #[test]
    fn deleting_an_unconsumed_item_leaves_the_cursor_alone() {
        let mut session = stack(&[3, 2, 1]);
        session.advance();
        session.remove_clip(ClipId(1));
        assert_eq!(session.cursor, 1);
        assert_eq!(session.next_item(), Some(ClipId(2)));
    }

    #[test]
    fn deleting_removes_every_occurrence() {
        let mut session = stack(&[1, 2, 1]);
        session.duplicate_policy = DuplicatePolicy::Preserve;
        assert!(session.remove_clip(ClipId(1)));
        assert_eq!(session.items, vec![ClipId(2)]);
    }
}
