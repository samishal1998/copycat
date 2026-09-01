//! The state machine every interface drives.
//!
//! `Core` never touches a clipboard. Paste commands return a [`PasteRequest`]
//! describing what should be written; the caller writes it and then calls
//! [`Core::commit_paste`] or [`Core::abort_paste`]. A cursor therefore advances
//! only on a write that actually succeeded — the failure case is the absence of
//! a confirmation rather than an error path someone had to remember.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::clip::{ClipId, ClipPayload, ClipSource, ClipSummary, ContentHash};
use crate::error::{CoreError, ErrorKind, Result};
use crate::history::{DuplicatePolicy, History, PREVIEW_CHARS};
use crate::session::{Session, SessionMode, SessionState, SessionSummary};

/// Window in which a clipboard change matching our own write counts as ours
/// (R16). Long enough for a slow compositor round trip, short enough that a
/// human cannot copy inside it by accident.
pub const DEFAULT_SUPPRESSION_WINDOW_MS: i64 = 750;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreConfig {
    pub hot_items: usize,
    pub duplicate_policy: DuplicatePolicy,
    pub group_delimiter: String,
    pub suppression_window_ms: i64,
    pub max_item_bytes: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            hot_items: 100,
            duplicate_policy: DuplicatePolicy::Collapse,
            group_delimiter: "\n".to_string(),
            suppression_window_ms: DEFAULT_SUPPRESSION_WINDOW_MS,
            max_item_bytes: 8 * 1024 * 1024,
        }
    }
}

/// What the core decided about an observed clipboard change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// A new external copy, recorded.
    Recorded { id: ClipId, entered_session: bool },
    /// Our own write, coming back at us (§10).
    Internal { token: u64 },
    /// Capture is paused; the value was seen and deliberately dropped.
    Paused,
    /// Nothing on the clipboard worth recording.
    Empty,
    /// Above `max_item_bytes`.
    TooLarge { bytes: usize },
}

/// What the caller should put on the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteRequest {
    pub payload: ClipPayload,
    pub hash: ContentHash,
    /// `None` for a group aggregate, which is transient and has no clip id
    /// (R13).
    pub clip_id: Option<ClipId>,
    /// Whether confirming this paste advances the active session.
    pub consuming: bool,
    /// Entries a group aggregation skipped for having no text (R14).
    pub skipped_non_text: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStarted {
    /// The session this one displaced, if any. Replacement is normal (R4).
    pub replaced: Option<SessionSummary>,
    pub session: SessionSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreStatus {
    pub paused: bool,
    pub hot_items: usize,
    pub hot_capacity: usize,
    pub duplicate_policy: DuplicatePolicy,
    /// Copycat's `offset 0`. After a paste this deliberately differs from what
    /// is on the OS clipboard (R15); the daemon reports both.
    pub latest: Option<ClipSummary>,
    pub session: Option<SessionSummary>,
}

#[derive(Debug, Clone, Copy)]
struct PendingWrite {
    hash: ContentHash,
    token: u64,
    deadline: i64,
}

#[derive(Debug, Clone, Copy)]
struct PendingPaste {
    consuming: bool,
}

#[derive(Debug)]
pub struct Core {
    config: CoreConfig,
    history: History,
    session: Option<Session>,
    paused: bool,
    next_session_id: u64,
    next_token: u64,
    pending_write: Option<PendingWrite>,
    pending_paste: Option<PendingPaste>,
}

impl Core {
    pub fn new(config: CoreConfig) -> Self {
        let history = History::new(config.hot_items);
        Core {
            config,
            history,
            session: None,
            paused: false,
            next_session_id: 1,
            next_token: 1,
            pending_write: None,
            pending_paste: None,
        }
    }

    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Bring persisted events back into hot history — at startup, or one at a
    /// time when an older clip is addressed by id.
    ///
    /// The restored events are protected for this trim, so pulling an aged-out
    /// clip back cannot immediately evict it again. They lose that protection
    /// afterwards, so the next copy trims them away normally.
    pub fn restore(&mut self, events: Vec<crate::clip::ClipEvent>) {
        let restored: BTreeSet<ClipId> = events.iter().map(|event| event.id).collect();
        self.history.restore(events);

        let mut protected = self.protected_ids();
        protected.extend(restored);
        self.history.evict(&protected);
    }

    // ---------------------------------------------------------------- capture

    /// Arm self-write suppression before writing to the system clipboard.
    ///
    /// Returns a token that identifies this write in the resulting
    /// [`Observation::Internal`], so logs can tie the two together.
    pub fn arm_suppression(&mut self, hash: ContentHash, at: i64) -> u64 {
        let token = self.next_token;
        self.next_token += 1;
        self.pending_write = Some(PendingWrite {
            hash,
            token,
            deadline: at + self.config.suppression_window_ms,
        });
        token
    }

    /// Classify a clipboard change and, if it is a genuine external copy,
    /// record it and offer it to the active session.
    pub fn observe(&mut self, payload: ClipPayload, at: i64) -> Observation {
        if payload.is_empty() {
            return Observation::Empty;
        }
        let hash = payload.content_hash();

        // Suppression is checked before the pause gate: a paste performed while
        // capture is paused must still not be mistaken for a copy later.
        if let Some(pending) = self.pending_write {
            if at > pending.deadline {
                self.pending_write = None;
            } else if pending.hash == hash {
                // Single-shot: platforms that fire several notifications for one
                // multi-format write only consume the record once, and the extra
                // notifications carry the same hash and are then treated as
                // external — which is why the deadline, not the count, bounds it.
                self.pending_write = None;
                return Observation::Internal { token: pending.token };
            }
        }

        if self.paused {
            return Observation::Paused;
        }
        let bytes = payload.byte_len();
        if bytes > self.config.max_item_bytes {
            return Observation::TooLarge { bytes };
        }

        // Resolve the collapse comparison before taking a mutable borrow of the
        // session: only the history can turn a clip id into a hash.
        let target_hash = self
            .session
            .as_ref()
            .and_then(Session::collapse_target)
            .and_then(|id| self.history.get(id))
            .map(|event| event.content_hash);
        let duplicates_target = target_hash == Some(hash);

        let id = self.history.append(payload, ClipSource::External, at);

        let entered_session = match self.session.as_mut() {
            Some(session) => session.accept(id, duplicates_target),
            None => false,
        };

        self.history.evict(&self.protected_ids());
        Observation::Recorded { id, entered_session }
    }

    fn protected_ids(&self) -> BTreeSet<ClipId> {
        match &self.session {
            Some(session) => session.items.iter().copied().collect(),
            None => BTreeSet::new(),
        }
    }

    // ------------------------------------------------------------ resolution

    fn policy(&self, raw: bool) -> DuplicatePolicy {
        if raw { DuplicatePolicy::Preserve } else { DuplicatePolicy::Collapse }
    }

    pub fn resolve_offset(&self, offset: usize, raw: bool) -> Result<ClipId> {
        self.history.resolve_offset(offset, self.policy(raw)).ok_or_else(|| {
            CoreError::not_found(
                "offset_out_of_range",
                format!(
                    "no clip at offset {offset}; {} in the {} view",
                    self.history.view(self.policy(raw)).len(),
                    if raw { "raw" } else { "collapsed" }
                ),
            )
        })
    }

    // ---------------------------------------------------------------- pasting

    /// Paste a specific clip. Never consuming: addressing an item by id or
    /// offset is not traversal (R12 covers the `--peek` case; this covers all
    /// direct addressing).
    pub fn begin_paste_clip(&mut self, id: ClipId) -> Result<PasteRequest> {
        let event = self
            .history
            .get(id)
            .ok_or_else(|| CoreError::not_found("clip_not_found", format!("no clip {id}")))?;
        let request = PasteRequest {
            payload: event.payload.clone(),
            hash: event.content_hash,
            clip_id: Some(id),
            consuming: false,
            skipped_non_text: 0,
        };
        self.pending_paste = Some(PendingPaste { consuming: false });
        Ok(request)
    }

    /// Paste the active session's next item.
    ///
    /// `peek` resolves the same item without arming the advance (R12).
    pub fn begin_paste_next(&mut self, peek: bool) -> Result<PasteRequest> {
        let session = self.session.as_ref().ok_or_else(|| {
            CoreError::not_found(
                "no_active_session",
                "no active session; start a stack or queue first",
            )
        })?;

        if session.mode == SessionMode::Group {
            return Err(CoreError::invalid(
                "group_not_traversable",
                "a group pastes as one value: use `group paste`",
            ));
        }
        if session.state == SessionState::Capturing {
            return Err(CoreError::invalid(
                "session_capturing",
                "the queue is still capturing: `queue seal` it first",
            ));
        }

        let id = session.next_item().ok_or_else(|| {
            CoreError::not_found(
                "session_exhausted",
                format!(
                    "{} session exhausted after {} items",
                    session.mode.as_str(),
                    session.items.len()
                ),
            )
        })?;

        let event = self.history.get(id).ok_or_else(|| {
            CoreError::not_found("clip_missing", format!("clip {id} is no longer available"))
        })?;

        let request = PasteRequest {
            payload: event.payload.clone(),
            hash: event.content_hash,
            clip_id: Some(id),
            consuming: !peek,
            skipped_non_text: 0,
        };
        self.pending_paste = Some(PendingPaste { consuming: !peek });
        Ok(request)
    }

    /// Aggregate the newest `n` logical entries into one payload.
    pub fn begin_paste_group_last(
        &mut self,
        n: usize,
        delimiter: Option<String>,
        raw: bool,
    ) -> Result<PasteRequest> {
        let ids = self.history.last_n(n, self.policy(raw));
        let delimiter = delimiter.unwrap_or_else(|| self.config.group_delimiter.clone());
        self.begin_paste_aggregate(&ids, &delimiter)
    }

    /// Aggregate everything the active group session captured.
    pub fn begin_paste_group_session(&mut self) -> Result<PasteRequest> {
        let session = self.session.as_ref().ok_or_else(|| {
            CoreError::not_found("no_active_session", "no active group; `group capture` first")
        })?;
        if session.mode != SessionMode::Group {
            return Err(CoreError::invalid(
                "not_a_group",
                format!("the active session is a {}, not a group", session.mode.as_str()),
            ));
        }
        let ids = session.items.clone();
        let delimiter = session
            .delimiter
            .clone()
            .unwrap_or_else(|| self.config.group_delimiter.clone());
        self.begin_paste_aggregate(&ids, &delimiter)
    }

    fn begin_paste_aggregate(&mut self, ids: &[ClipId], delimiter: &str) -> Result<PasteRequest> {
        if ids.is_empty() {
            return Err(CoreError::not_found(
                "nothing_to_group",
                "no clips available to group",
            ));
        }

        let mut parts = Vec::with_capacity(ids.len());
        let mut skipped = 0usize;
        for id in ids {
            match self.history.get(*id).and_then(|e| e.payload.as_text()) {
                Some(text) => parts.push(text.to_string()),
                // Skipping an image rather than failing keeps a mixed capture
                // usable; the count is reported so it is never silent (R14).
                None => skipped += 1,
            }
        }
        if parts.is_empty() {
            return Err(CoreError::unsupported(
                "group_no_text",
                format!("none of the {} selected clips have text", ids.len()),
            ));
        }

        let payload = ClipPayload::text(parts.join(delimiter));
        let hash = payload.content_hash();
        // Not consuming, and carries no clip id: the aggregate is transient and
        // is never recorded as history (R13).
        self.pending_paste = Some(PendingPaste { consuming: false });
        Ok(PasteRequest {
            payload,
            hash,
            clip_id: None,
            consuming: false,
            skipped_non_text: skipped,
        })
    }

    /// The write and the injection both succeeded.
    pub fn commit_paste(&mut self) -> Option<SessionSummary> {
        let pending = self.pending_paste.take()?;
        if !pending.consuming {
            return self.session.as_ref().map(Session::summary);
        }
        let session = self.session.as_mut()?;
        session.advance();
        Some(session.summary())
    }

    /// The write or the injection failed: leave the cursor where it was.
    pub fn abort_paste(&mut self) {
        self.pending_paste = None;
    }

    // --------------------------------------------------------------- sessions

    fn install(&mut self, session: Session) -> SessionStarted {
        let replaced = self.session.take().as_ref().map(Session::summary);
        let summary = session.summary();
        self.session = Some(session);
        // A new session may protect ids the old one did not; conversely the old
        // session's protection is gone, so this is where over-capacity history
        // finally gets trimmed.
        self.history.evict(&self.protected_ids());
        SessionStarted { replaced, session: summary }
    }

    fn new_session_id(&mut self) -> u64 {
        let id = self.next_session_id;
        self.next_session_id += 1;
        id
    }

    pub fn stack_start(&mut self, duplicates: DuplicatePolicy, at: i64) -> SessionStarted {
        let items = self.history.view(duplicates).into_iter().map(|e| e.id).collect();
        let id = self.new_session_id();
        self.install(Session::new(
            id,
            SessionMode::Stack,
            duplicates,
            SessionState::Ready,
            items,
            None,
            at,
        ))
    }

    pub fn queue_start_last(
        &mut self,
        n: usize,
        duplicates: DuplicatePolicy,
        at: i64,
    ) -> Result<SessionStarted> {
        if n == 0 {
            return Err(CoreError::invalid("empty_queue", "--last must be at least 1"));
        }
        let items = self.history.last_n(n, duplicates);
        if items.is_empty() {
            return Err(CoreError::not_found("history_empty", "no clips to queue"));
        }
        let id = self.new_session_id();
        Ok(self.install(Session::new(
            id,
            SessionMode::Queue,
            duplicates,
            SessionState::Ready,
            items,
            None,
            at,
        )))
    }

    pub fn queue_capture(&mut self, duplicates: DuplicatePolicy, at: i64) -> SessionStarted {
        let id = self.new_session_id();
        self.install(Session::new(
            id,
            SessionMode::Queue,
            duplicates,
            SessionState::Capturing,
            Vec::new(),
            None,
            at,
        ))
    }

    pub fn queue_seal(&mut self) -> Result<SessionSummary> {
        let session = self.session.as_mut().ok_or_else(|| {
            CoreError::not_found("no_active_session", "no queue to seal")
        })?;
        if session.mode != SessionMode::Queue {
            return Err(CoreError::invalid(
                "not_a_queue",
                format!("the active session is a {}", session.mode.as_str()),
            ));
        }
        if session.state != SessionState::Capturing {
            return Err(CoreError::invalid("already_sealed", "the queue is already sealed"));
        }
        session.seal();
        Ok(session.summary())
    }

    pub fn group_capture(
        &mut self,
        delimiter: Option<String>,
        duplicates: DuplicatePolicy,
        at: i64,
    ) -> SessionStarted {
        let id = self.new_session_id();
        self.install(Session::new(
            id,
            SessionMode::Group,
            duplicates,
            SessionState::Capturing,
            Vec::new(),
            Some(delimiter.unwrap_or_else(|| self.config.group_delimiter.clone())),
            at,
        ))
    }

    pub fn session_stop(&mut self) -> Option<SessionSummary> {
        let summary = self.session.take().as_ref().map(Session::summary);
        self.history.evict(&self.protected_ids());
        summary
    }

    pub fn session_reset(&mut self) -> Result<SessionSummary> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| CoreError::not_found("no_active_session", "no session to reset"))?;
        session.reset();
        Ok(session.summary())
    }

    // ---------------------------------------------------------------- history

    pub fn delete(&mut self, id: ClipId) -> Result<()> {
        if !self.history.delete(id) {
            return Err(CoreError::not_found("clip_not_found", format!("no clip {id}")));
        }
        if let Some(session) = self.session.as_mut() {
            session.remove_clip(id);
        }
        Ok(())
    }

    pub fn clear(&mut self, keep_pinned: bool) -> usize {
        let removed = self.history.clear(keep_pinned);
        if let Some(session) = self.session.as_mut() {
            for id in &removed {
                session.remove_clip(*id);
            }
        }
        removed.len()
    }

    pub fn set_pinned(&mut self, id: ClipId, pinned: bool) -> Result<()> {
        if self.history.set_pinned(id, pinned) {
            Ok(())
        } else {
            Err(CoreError::not_found("clip_not_found", format!("no clip {id}")))
        }
    }

    pub fn status(&self) -> CoreStatus {
        CoreStatus {
            paused: self.paused,
            hot_items: self.history.len(),
            hot_capacity: self.history.capacity(),
            duplicate_policy: self.config.duplicate_policy,
            latest: self
                .history
                .view(self.config.duplicate_policy)
                .first()
                .and_then(|entry| self.history.get(entry.id))
                .map(|event| event.summary(PREVIEW_CHARS)),
            session: self.session.as_ref().map(Session::summary),
        }
    }
}

impl From<CoreError> for ErrorKind {
    fn from(error: CoreError) -> Self {
        error.kind
    }
}
