//! The ordering invariants that define the product (§15.1).
//!
//! Every test here drives the full paste loop — resolve, arm suppression,
//! let the watcher see our own write, confirm — because that loop is where
//! ordering actually breaks. A test that called `commit_paste` without
//! replaying the watcher observation would pass while the daemon corrupted
//! its own history.

use copycat_core::{
    ClipId, ClipPayload, Core, CoreConfig, DuplicatePolicy, Observation, SessionMode,
};

/// Drives a `Core` the way the daemon does, with a clock the test controls.
struct Harness {
    core: Core,
    now: i64,
}

impl Harness {
    fn new() -> Self {
        Harness::with_config(CoreConfig::default())
    }

    fn with_config(config: CoreConfig) -> Self {
        Harness { core: Core::new(config), now: 1_000 }
    }

    fn tick(&mut self) -> i64 {
        self.now += 10;
        self.now
    }

    fn copy(&mut self, text: &str) -> Observation {
        let at = self.tick();
        self.core.observe(ClipPayload::text(text), at)
    }

    /// A full successful paste: write it, watch it come back, confirm.
    fn paste_next(&mut self) -> Result<String, String> {
        let request = self.core.begin_paste_next(false).map_err(|e| e.code)?;
        let text = request.payload.as_text().unwrap().to_string();
        let at = self.tick();
        self.core.arm_suppression(request.hash, at);
        let echoed_at = self.tick();
        let echo = self.core.observe(request.payload.clone(), echoed_at);
        assert!(
            matches!(echo, Observation::Internal { .. }),
            "our own write must never become history"
        );
        self.core.commit_paste();
        Ok(text)
    }

    /// A paste whose clipboard write failed: nothing is confirmed.
    fn paste_next_failing(&mut self) -> Result<String, String> {
        let request = self.core.begin_paste_next(false).map_err(|e| e.code)?;
        let text = request.payload.as_text().unwrap().to_string();
        self.core.abort_paste();
        Ok(text)
    }

    fn drain(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(text) = self.paste_next() {
            out.push(text);
            if out.len() > 64 {
                panic!("traversal did not terminate — it wrapped");
            }
        }
        out
    }

    fn latest_text(&self) -> Option<String> {
        self.core.status().latest.map(|s| s.preview)
    }
}

// ------------------------------------------------------------------- stack

#[test]
fn collapsed_stack_never_emits_consecutive_equal_values() {
    let mut h = Harness::new();
    for value in ["A", "A", "A", "B", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    let pasted = h.drain();

    assert_eq!(pasted, ["C", "B", "A"]);
    assert!(
        pasted.windows(2).all(|w| w[0] != w[1]),
        "collapse must leave no adjacent repeats"
    );
    assert_eq!(h.core.history().len(), 6, "the raw log keeps all six events");
}

#[test]
fn preserved_stack_keeps_multiplicity() {
    let mut h = Harness::new();
    for value in ["A", "A", "A", "B", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Preserve, h.now);

    assert_eq!(h.drain(), ["C", "B", "B", "A", "A", "A"]);
}

#[test]
fn a_copy_during_an_active_stack_becomes_the_next_item() {
    // The PRD's worked example: copy D once the stack has advanced to B.
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    assert_eq!(h.paste_next().unwrap(), "C");
    h.copy("D");

    assert_eq!(h.drain(), ["D", "B", "A"]);
}

#[test]
fn traversing_past_the_end_reports_exhaustion_rather_than_wrapping() {
    // R6.
    let mut h = Harness::new();
    h.copy("A");
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    assert_eq!(h.paste_next().unwrap(), "A");
    assert_eq!(h.paste_next().unwrap_err(), "session_exhausted");
}

#[test]
fn traversal_without_a_session_is_an_error() {
    // R11.
    let mut h = Harness::new();
    h.copy("A");
    assert_eq!(h.paste_next().unwrap_err(), "no_active_session");
}

// ------------------------------------------------------------------- queue

#[test]
fn queue_last_n_is_fifo_and_is_a_snapshot() {
    let mut h = Harness::new();
    for value in ["A", "B", "C", "D", "E"] {
        h.copy(value);
    }
    h.core.queue_start_last(5, DuplicatePolicy::Collapse, h.now).unwrap();

    assert_eq!(h.paste_next().unwrap(), "A");
    h.copy("F");

    assert_eq!(h.drain(), ["B", "C", "D", "E"], "a sealed queue must not drift");
}

#[test]
fn queue_last_n_takes_what_exists() {
    // R3.
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.queue_start_last(5, DuplicatePolicy::Collapse, h.now).unwrap();
    assert_eq!(h.drain(), ["A", "B", "C"]);
}

#[test]
fn queue_capture_pastes_in_capture_order_and_must_be_sealed_first() {
    let mut h = Harness::new();
    h.copy("ignored");
    h.core.queue_capture(DuplicatePolicy::Collapse, h.now);
    for value in ["A", "B", "C"] {
        h.copy(value);
    }

    assert_eq!(h.paste_next().unwrap_err(), "session_capturing");
    h.core.queue_seal().unwrap();

    assert_eq!(h.drain(), ["A", "B", "C"]);
}

#[test]
fn queue_capture_collapses_against_the_tail() {
    // R8.
    let mut h = Harness::new();
    h.core.queue_capture(DuplicatePolicy::Collapse, h.now);
    for value in ["A", "A", "B", "A"] {
        h.copy(value);
    }
    h.core.queue_seal().unwrap();

    assert_eq!(h.drain(), ["A", "B", "A"]);
}

// ------------------------------------------------------------------- group

#[test]
fn group_last_n_joins_chronologically_with_the_delimiter() {
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    let request = h.core.begin_paste_group_last(3, Some(", ".into()), false).unwrap();

    assert_eq!(request.payload.as_text(), Some("A, B, C"));
    assert_eq!(request.clip_id, None, "an aggregate has no clip id");
}

#[test]
fn a_group_aggregate_is_transient_and_never_becomes_history() {
    // R13: the aggregate must not show up as a new clip.
    let mut h = Harness::new();
    for value in ["A", "B"] {
        h.copy(value);
    }
    let before = h.core.history().len();

    let request = h.core.begin_paste_group_last(2, None, false).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);
    let echoed_at = h.tick();
    let echo = h.core.observe(request.payload.clone(), echoed_at);
    h.core.commit_paste();

    assert!(matches!(echo, Observation::Internal { .. }));
    assert_eq!(h.core.history().len(), before);
    assert_eq!(h.latest_text().as_deref(), Some("B"));
}

#[test]
fn group_capture_preserves_capture_order() {
    let mut h = Harness::new();
    h.core.group_capture(Some("|".into()), DuplicatePolicy::Collapse, h.now);
    for value in ["one", "two", "three"] {
        h.copy(value);
    }

    let request = h.core.begin_paste_group_session().unwrap();
    assert_eq!(request.payload.as_text(), Some("one|two|three"));
}

#[test]
fn group_skips_entries_without_text_instead_of_failing() {
    // R14.
    use copycat_core::Representation;
    let mut h = Harness::new();
    h.copy("A");
    let at = h.tick();
    h.core.observe(
        ClipPayload {
            representations: vec![Representation {
                media_type: "image/png".into(),
                bytes: vec![1, 2, 3],
            }],
        },
        at,
    );
    h.copy("B");

    let request = h.core.begin_paste_group_last(3, Some("-".into()), false).unwrap();

    assert_eq!(request.payload.as_text(), Some("A-B"));
    assert_eq!(request.skipped_non_text, 1);
}

#[test]
fn group_fails_only_when_nothing_selected_has_text() {
    use copycat_core::Representation;
    let mut h = Harness::new();
    let at = h.tick();
    h.core.observe(
        ClipPayload {
            representations: vec![Representation {
                media_type: "image/png".into(),
                bytes: vec![1],
            }],
        },
        at,
    );
    let error = h.core.begin_paste_group_last(1, None, false).unwrap_err();
    assert_eq!(error.code, "group_no_text");
    assert_eq!(error.exit_code(), 8);
}

#[test]
fn a_group_session_cannot_be_traversed() {
    let mut h = Harness::new();
    h.copy("A");
    h.core.group_capture(None, DuplicatePolicy::Collapse, h.now);
    h.copy("B");
    assert_eq!(h.paste_next().unwrap_err(), "group_not_traversable");
}

// ------------------------------------------------------------- paste effects

#[test]
fn a_failed_paste_does_not_advance_the_cursor() {
    let mut h = Harness::new();
    for value in ["A", "B"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    assert_eq!(h.paste_next_failing().unwrap(), "B");
    assert_eq!(h.paste_next_failing().unwrap(), "B", "still the same item");
    assert_eq!(h.drain(), ["B", "A"]);
}

#[test]
fn peeking_resolves_the_next_item_without_consuming_it() {
    // R12.
    let mut h = Harness::new();
    for value in ["A", "B"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    let peek = h.core.begin_paste_next(true).unwrap();
    h.core.commit_paste();

    assert_eq!(peek.payload.as_text(), Some("B"));
    assert_eq!(h.drain(), ["B", "A"]);
}

#[test]
fn addressing_a_clip_by_offset_never_consumes_a_session() {
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);

    let id = h.core.resolve_offset(2, false).unwrap();
    h.core.begin_paste_clip(id).unwrap();
    h.core.commit_paste();

    assert_eq!(h.drain(), ["C", "B", "A"]);
}

#[test]
fn the_os_clipboard_and_offset_zero_diverge_after_a_paste() {
    // R15: this is intended behaviour, and the test exists so nobody
    // "fixes" it into recording our own writes.
    let mut h = Harness::new();
    for value in ["A", "B"] {
        h.copy(value);
    }
    let id = h.core.resolve_offset(1, false).unwrap();
    let request = h.core.begin_paste_clip(id).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);
    let echoed_at = h.tick();
    h.core.observe(request.payload.clone(), echoed_at);
    h.core.commit_paste();

    assert_eq!(request.payload.as_text(), Some("A"), "the OS clipboard now holds A");
    assert_eq!(h.latest_text().as_deref(), Some("B"), "offset 0 is still B");
}

// -------------------------------------------------------------- suppression

#[test]
fn an_identical_copy_after_the_window_closes_is_external() {
    let mut h = Harness::new();
    h.copy("A");
    let request = h.core.begin_paste_clip(ClipId(1)).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);
    h.core.commit_paste();

    let late = at + copycat_core::DEFAULT_SUPPRESSION_WINDOW_MS + 1;
    let observation = h.core.observe(request.payload, late);

    assert!(matches!(observation, Observation::Recorded { .. }));
}

#[test]
fn a_different_copy_inside_the_window_is_still_external() {
    let mut h = Harness::new();
    h.copy("A");
    let request = h.core.begin_paste_clip(ClipId(1)).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);

    let observation = h.core.observe(ClipPayload::text("typed by the user"), at + 5);

    assert!(matches!(observation, Observation::Recorded { .. }));
}

#[test]
fn suppression_is_single_shot() {
    // A platform that fires twice for one multi-format write consumes the
    // record once; the second notification is indistinguishable from a real
    // copy and is recorded (R17, stated rather than hidden).
    let mut h = Harness::new();
    h.copy("A");
    let request = h.core.begin_paste_clip(ClipId(1)).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);

    let first = h.core.observe(request.payload.clone(), at + 1);
    let second = h.core.observe(request.payload, at + 2);

    assert!(matches!(first, Observation::Internal { .. }));
    assert!(matches!(second, Observation::Recorded { .. }));
}

#[test]
fn paused_capture_drops_copies_but_still_suppresses_our_writes() {
    let mut h = Harness::new();
    h.copy("A");
    h.core.set_paused(true);

    assert!(matches!(h.copy("B"), Observation::Paused));

    let request = h.core.begin_paste_clip(ClipId(1)).unwrap();
    let at = h.tick();
    h.core.arm_suppression(request.hash, at);
    assert!(matches!(
        h.core.observe(request.payload, at + 1),
        Observation::Internal { .. }
    ));
    assert_eq!(h.core.history().len(), 1);
}

// ----------------------------------------------------------------- sessions

#[test]
fn starting_a_mode_replaces_the_active_session() {
    // R4: replacement is normal, not an error.
    let mut h = Harness::new();
    h.copy("A");
    let first = h.core.stack_start(DuplicatePolicy::Collapse, h.now);
    assert!(first.replaced.is_none());

    let second = h.core.queue_capture(DuplicatePolicy::Collapse, h.now);

    let replaced = second.replaced.expect("the stack should be reported as replaced");
    assert_eq!(replaced.mode, SessionMode::Stack);
    assert_eq!(h.core.session().unwrap().mode, SessionMode::Queue);
}

#[test]
fn deleting_a_consumed_clip_keeps_the_rest_of_the_traversal_intact() {
    // R9.
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);
    assert_eq!(h.paste_next().unwrap(), "C");

    let consumed = h.core.resolve_offset(0, false).unwrap();
    h.core.delete(consumed).unwrap();

    assert_eq!(h.drain(), ["B", "A"]);
}

#[test]
fn deleting_an_upcoming_clip_removes_it_from_the_traversal() {
    let mut h = Harness::new();
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);
    let upcoming = h.core.resolve_offset(1, false).unwrap();

    h.core.delete(upcoming).unwrap();

    assert_eq!(h.drain(), ["C", "A"]);
}

#[test]
fn an_active_session_survives_copies_that_would_have_evicted_its_items() {
    // R10: a two-item hot history, a sealed two-item queue, then three more
    // copies. Without protection the queue would traverse into nothing.
    let mut h = Harness::with_config(CoreConfig { hot_items: 2, ..CoreConfig::default() });
    h.copy("A");
    h.copy("B");
    h.core.queue_start_last(2, DuplicatePolicy::Collapse, h.now).unwrap();

    for value in ["C", "D", "E"] {
        h.copy(value);
    }

    assert_eq!(h.drain(), ["A", "B"]);
}

#[test]
fn restoring_an_aged_out_clip_does_not_immediately_evict_it_again() {
    // The daemon pulls a clip back from the store to paste it by id. If that
    // restore enforced the cap itself, it would pop the very event it fetched:
    // restored clips are older, so they sort to the front of the queue.
    let mut h = Harness::with_config(CoreConfig { hot_items: 2, ..CoreConfig::default() });
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    let aged_out = ClipId(1);
    assert!(h.core.history().get(aged_out).is_none(), "A should have been evicted");

    h.core.restore(vec![copycat_core::ClipEvent {
        id: aged_out,
        captured_at: 0,
        source: copycat_core::ClipSource::External,
        content_hash: ClipPayload::text("A").content_hash(),
        payload: ClipPayload::text("A"),
        pinned: false,
    }]);

    let request = h.core.begin_paste_clip(aged_out).expect("the restored clip must be pastable");
    assert_eq!(request.payload.as_text(), Some("A"));
}

#[test]
fn restoring_never_drops_a_pinned_or_session_referenced_event() {
    // Reaching the R10 violation needs hot history legitimately over capacity
    // through protection first: only then does trimming continue past the
    // restored event and into items the session still needs.
    let mut h = Harness::with_config(CoreConfig { hot_items: 2, ..CoreConfig::default() });
    for value in ["A", "B", "C"] {
        h.copy(value);
    }
    h.core.stack_start(DuplicatePolicy::Collapse, h.now);
    for value in ["D", "E"] {
        h.copy(value);
    }
    let session_items = h.core.session().unwrap().items.clone();
    assert!(session_items.len() > 2, "the session should hold hot history over capacity");
    h.core.set_pinned(session_items[0], true).unwrap();

    h.core.restore(vec![copycat_core::ClipEvent {
        id: ClipId(1),
        captured_at: 0,
        source: copycat_core::ClipSource::External,
        content_hash: ClipPayload::text("A").content_hash(),
        payload: ClipPayload::text("A"),
        pinned: false,
    }]);

    for id in &session_items {
        assert!(h.core.history().get(*id).is_some(), "session item {id} was evicted");
    }
    assert_eq!(h.drain(), ["E", "D", "C", "B"], "and the traversal still completes");
}

#[test]
fn ending_a_session_releases_its_protection() {
    let mut h = Harness::with_config(CoreConfig { hot_items: 2, ..CoreConfig::default() });
    h.copy("A");
    h.copy("B");
    h.core.queue_start_last(2, DuplicatePolicy::Collapse, h.now).unwrap();
    for value in ["C", "D", "E"] {
        h.copy(value);
    }
    assert_eq!(h.core.history().len(), 4, "A and B are held open by the queue");

    h.core.session_stop();

    assert_eq!(h.core.history().len(), 2, "and released when it ends");
}

// -------------------------------------------------------- randomized streams

/// A tiny deterministic PRNG. A dependency would buy nothing here: the tests
/// need reproducible pseudo-randomness, not statistical quality.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_streams_hold_the_stack_invariants() {
    // For every seed: a collapsed stack emits no adjacent repeats, a preserved
    // stack emits exactly the reversed log, and neither ever loses or invents
    // an item.
    let alphabet = ["A", "B", "C", "D"];

    for seed in 1..200u64 {
        let mut rng = Rng(seed);
        let mut copied: Vec<&str> = Vec::new();
        let mut h = Harness::new();

        for _ in 0..rng.below(20) + 1 {
            let value = alphabet[rng.below(alphabet.len())];
            copied.push(value);
            h.copy(value);
        }

        let preserve = rng.next() % 2 == 0;
        let policy = if preserve { DuplicatePolicy::Preserve } else { DuplicatePolicy::Collapse };
        h.core.stack_start(policy, h.now);
        let pasted = h.drain();

        if preserve {
            let mut expected = copied.clone();
            expected.reverse();
            assert_eq!(pasted, expected, "seed {seed}");
        } else {
            assert!(
                pasted.windows(2).all(|w| w[0] != w[1]),
                "seed {seed}: collapsed stack emitted an adjacent repeat: {pasted:?}"
            );
            let mut deduped: Vec<&str> = Vec::new();
            for value in &copied {
                if deduped.last() != Some(value) {
                    deduped.push(value);
                }
            }
            deduped.reverse();
            assert_eq!(pasted, deduped, "seed {seed}");
        }

        assert_eq!(h.core.history().len(), copied.len(), "seed {seed}: raw log is untouched");
    }
}

#[test]
fn random_interleavings_never_lose_or_duplicate_a_stack_item() {
    // Copies interleaved with pastes: whatever the schedule, every value
    // copied before the stack drains must come out exactly once, and the
    // stack must terminate.
    for seed in 1..200u64 {
        let mut rng = Rng(seed);
        let mut h = Harness::new();
        let mut unique = 0usize;

        h.copy("seed");
        h.core.stack_start(DuplicatePolicy::Preserve, h.now);
        unique += 1;

        let mut pasted = Vec::new();
        for step in 0..rng.below(24) + 1 {
            if rng.next() % 3 == 0 {
                h.copy(&format!("v{seed}-{step}"));
                unique += 1;
            } else if let Ok(text) = h.paste_next() {
                pasted.push(text);
            }
        }
        pasted.extend(h.drain());

        assert_eq!(pasted.len(), unique, "seed {seed}: items lost or duplicated");
        let mut sorted = pasted.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), pasted.len(), "seed {seed}: an item came out twice");
    }
}
