//! Daemon integration tests (§15.3).
//!
//! These run a real `copycatd` over a real socket with the file-backed
//! clipboard, so they cover the parts unit tests structurally cannot: the
//! watcher, framing, persistence, and the paste transaction as one path.
//!
//! Nothing here sleeps for a fixed duration and hopes. Every step polls for the
//! state it needs and fails with a message if it never arrives, so a slow
//! machine makes the suite slower rather than flaky.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use copycat_core::{ClipId, ClipSummary, DuplicatePolicy};
use copycat_protocol::{Action, ResultBody};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Daemon {
    child: Child,
    socket: PathBuf,
    run: PathBuf,
    clipboard: PathBuf,
    #[allow(dead_code)]
    data: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let data = tempfile::tempdir().expect("temp dir");
        // The daemon refuses a data directory other users can enter, and this
        // machine's umask leaves temp directories at 0775.
        make_private(data.path());
        let clipboard = data.path().join("clipboard.txt");
        // Two constraints meet here: a Unix socket path is capped at ~108
        // bytes, which a nested temp directory can exceed, and the daemon
        // refuses a socket directory other users can enter. So the socket gets
        // its own short, private directory.
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let run = std::env::temp_dir().join(format!(
            "cc-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&run).expect("run dir");
        make_private(&run);
        let socket = run.join("d.sock");

        let daemon = Daemon {
            child: spawn(&socket, data.path(), &clipboard),
            socket,
            run,
            clipboard,
            data,
        };
        daemon.await_socket();
        daemon
    }

    /// Restart against the same data directory, to test what survives.
    fn restart(&mut self) {
        self.call(Action::DaemonStop).expect("stop");
        let _ = self.child.wait();
        await_gone(&self.socket);
        self.child = spawn(&self.socket, self.data.path(), &self.clipboard);
        self.await_socket();
    }

    fn await_socket(&self) {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if copycat_protocol::is_running(&self.socket) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the daemon never began listening on {}", self.socket.display());
    }

    fn call(&self, action: Action) -> Result<ResultBody, copycat_core::CoreError> {
        copycat_protocol::call(&self.socket, action)
    }

    fn ok(&self, action: Action) -> ResultBody {
        self.call(action).expect("action should succeed")
    }

    /// Simulate another application copying, and wait until it is recorded.
    fn copy(&self, text: &str) {
        let before = self.raw_history().len();
        let mut file = std::fs::File::create(&self.clipboard).expect("write clipboard");
        file.write_all(text.as_bytes()).expect("write clipboard");
        file.sync_all().ok();
        drop(file);

        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            let history = self.raw_history();
            if history.len() > before {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("copying {text:?} was never recorded");
    }

    fn history(&self, raw: bool) -> Vec<ClipSummary> {
        match self.ok(Action::HistoryList { limit: 1000, raw }) {
            ResultBody::Clips { clips, .. } => clips,
            other => panic!("expected clips, got {other:?}"),
        }
    }

    fn raw_history(&self) -> Vec<ClipSummary> {
        self.history(true)
    }

    fn previews(&self, raw: bool) -> Vec<String> {
        self.history(raw).into_iter().map(|c| c.preview).collect()
    }

    /// Paste and return what landed on the clipboard.
    fn paste_next(&self) -> Result<String, String> {
        match self.call(Action::PasteNext { peek: false }) {
            Ok(ResultBody::Pasted { preview, .. }) => Ok(preview),
            Ok(other) => panic!("expected a paste, got {other:?}"),
            Err(error) => Err(error.code),
        }
    }

    fn clipboard_contents(&self) -> String {
        std::fs::read_to_string(&self.clipboard).unwrap_or_default()
    }

    fn drain(&self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(text) = self.paste_next() {
            out.push(text);
            assert!(out.len() < 64, "traversal did not terminate");
        }
        out
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.run);
    }
}

fn make_private(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn spawn(socket: &Path, data: &Path, clipboard: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_copycatd"))
        .arg("--socket").arg(socket)
        .arg("--data-dir").arg(data)
        .arg("--config").arg(data.join("config.toml"))
        .arg("--clipboard").arg("file")
        .arg("--clipboard-file").arg(clipboard)
        .arg("--log").arg("warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("copycatd should start")
}

fn await_gone(socket: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if !copycat_protocol::is_running(socket) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the daemon never stopped listening");
}

// ---------------------------------------------------------------------------

#[test]
fn copies_are_captured_and_repeats_reach_the_raw_log() {
    let daemon = Daemon::start();
    for value in ["A", "B", "B", "C"] {
        daemon.copy(value);
    }

    assert_eq!(daemon.previews(true), ["C", "B", "B", "A"], "the raw log keeps the repeat");
    assert_eq!(daemon.previews(false), ["C", "B", "A"], "the collapsed view folds it");

    let collapsed = daemon.history(false);
    assert_eq!(collapsed[1].duplicate_run, 2, "and reports how many it folded");
}

#[test]
fn a_stack_traverses_lifo_and_takes_new_copies_at_the_cursor() {
    let daemon = Daemon::start();
    for value in ["A", "B", "C"] {
        daemon.copy(value);
    }
    daemon.ok(Action::StackStart { duplicates: None });

    assert_eq!(daemon.paste_next().unwrap(), "C");
    daemon.copy("D");

    assert_eq!(daemon.drain(), ["D", "B", "A"]);
    assert_eq!(daemon.paste_next().unwrap_err(), "session_exhausted");
}

#[test]
fn a_sealed_queue_is_a_snapshot_that_new_copies_do_not_join() {
    let daemon = Daemon::start();
    for value in ["A", "B", "C"] {
        daemon.copy(value);
    }
    daemon.ok(Action::QueueStart { last: 3, duplicates: None });

    assert_eq!(daemon.paste_next().unwrap(), "A");
    daemon.copy("D");

    assert_eq!(daemon.drain(), ["B", "C"]);
}

#[test]
fn a_capture_queue_must_be_sealed_before_it_traverses() {
    let daemon = Daemon::start();
    daemon.ok(Action::QueueCapture { duplicates: None });
    for value in ["one", "two", "three"] {
        daemon.copy(value);
    }

    assert_eq!(daemon.paste_next().unwrap_err(), "session_capturing");
    daemon.ok(Action::QueueSeal);

    assert_eq!(daemon.drain(), ["one", "two", "three"]);
}

#[test]
fn a_group_aggregate_reaches_the_clipboard_without_entering_history() {
    let daemon = Daemon::start();
    for value in ["one", "two", "three"] {
        daemon.copy(value);
    }
    let before = daemon.raw_history().len();

    let result = daemon.ok(Action::GroupPasteLast {
        last: 3,
        delimiter: Some(", ".into()),
        raw: false,
    });
    let ResultBody::Pasted { clip_id, .. } = &result else { panic!("expected a paste") };
    assert_eq!(*clip_id, None, "an aggregate has no clip id");

    assert_eq!(daemon.clipboard_contents(), "one, two, three");
    assert_eq!(daemon.raw_history().len(), before, "and is never recorded");
}

#[test]
fn our_own_writes_never_become_history() {
    // The self-write suppression path, end to end through the real watcher.
    let daemon = Daemon::start();
    daemon.copy("A");
    daemon.copy("B");
    let before = daemon.raw_history();

    daemon.ok(Action::PasteOffset { offset: 1, raw: false });
    assert_eq!(daemon.clipboard_contents(), "A");

    // Give the watcher several poll intervals to misbehave if it is going to.
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(daemon.raw_history().len(), before.len(), "the paste must not be a new copy");
    assert_eq!(daemon.previews(false)[0], "B", "offset 0 is still the last real copy");
}

#[test]
fn history_and_pins_survive_a_restart() {
    let mut daemon = Daemon::start();
    daemon.copy("first");
    daemon.copy("second");
    let pinned = daemon.history(true)[0].id;
    daemon.ok(Action::HistoryPin { id: pinned, pinned: true });

    daemon.restart();

    let restored = daemon.history(true);
    assert_eq!(
        restored.iter().map(|c| c.preview.clone()).collect::<Vec<_>>(),
        ["second", "first"]
    );
    assert!(restored[0].pinned, "a pin must survive too");
}

#[test]
fn persisted_payloads_are_not_readable_in_the_database() {
    let mut daemon = Daemon::start();
    daemon.copy("correct-horse-battery-staple");
    daemon.restart(); // force a flush and reopen

    let database = std::fs::read(daemon.data.path().join("history.sqlite3")).expect("database");
    let text = String::from_utf8_lossy(&database);
    assert!(
        !text.contains("correct-horse-battery-staple"),
        "payload text must not be readable in the database file"
    );
}

#[test]
fn clearing_can_spare_pinned_clips() {
    let daemon = Daemon::start();
    for value in ["keep", "drop-1", "drop-2"] {
        daemon.copy(value);
    }
    let keep = daemon.history(true).last().unwrap().id;
    daemon.ok(Action::HistoryPin { id: keep, pinned: true });

    daemon.ok(Action::HistoryClear { keep_pinned: true });

    assert_eq!(daemon.previews(true), ["keep"]);
}

#[test]
fn paused_capture_records_nothing_and_resumes_cleanly() {
    let daemon = Daemon::start();
    daemon.copy("before");
    daemon.ok(Action::HistoryPause);

    std::fs::write(&daemon.clipboard, "while paused").unwrap();
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(daemon.previews(true), ["before"]);

    daemon.ok(Action::HistoryResume);
    daemon.copy("after");
    assert_eq!(daemon.previews(true), ["after", "before"]);
}

#[test]
fn search_finds_persisted_text() {
    let daemon = Daemon::start();
    for value in ["postgres://localhost", "redis://cache", "postgres pool size"] {
        daemon.copy(value);
    }
    let ResultBody::Clips { clips, .. } =
        daemon.ok(Action::HistorySearch { query: "POSTGRES".into(), limit: 10 })
    else {
        panic!("expected clips")
    };
    assert_eq!(clips.len(), 2, "search is case-insensitive");
}

#[test]
fn deleting_a_clip_removes_it_from_the_active_session() {
    // R9, through the real socket rather than in-process.
    let daemon = Daemon::start();
    for value in ["A", "B", "C"] {
        daemon.copy(value);
    }
    daemon.ok(Action::StackStart { duplicates: None });
    let upcoming = daemon.history(false)[1].id; // "B"

    daemon.ok(Action::HistoryDelete { id: upcoming });

    assert_eq!(daemon.drain(), ["C", "A"]);
}

#[test]
fn starting_a_mode_replaces_the_active_session() {
    let daemon = Daemon::start();
    daemon.copy("A");
    daemon.ok(Action::StackStart { duplicates: None });

    let ResultBody::SessionStarted(started) = daemon.ok(Action::QueueCapture { duplicates: None })
    else {
        panic!("expected a session")
    };
    assert!(started.replaced.is_some(), "replacement is reported, not an error");
}

#[test]
fn preserve_keeps_a_repeat_that_collapse_would_fold() {
    // The end-to-end proof that duplicate policy means something: it only does
    // if the watcher can see a repeat copy at all.
    let daemon = Daemon::start();
    for value in ["A", "A", "B"] {
        daemon.copy(value);
    }

    daemon.ok(Action::StackStart { duplicates: Some(DuplicatePolicy::Preserve) });
    assert_eq!(daemon.drain(), ["B", "A", "A"]);

    daemon.ok(Action::StackStart { duplicates: Some(DuplicatePolicy::Collapse) });
    assert_eq!(daemon.drain(), ["B", "A"]);
}

#[test]
fn unknown_clips_and_versions_are_refused_with_useful_codes() {
    let daemon = Daemon::start();
    let error = daemon.call(Action::HistoryShow { id: ClipId(999) }).unwrap_err();
    assert_eq!(error.code, "clip_not_found");
    assert_eq!(error.exit_code(), 7);

    let mismatched = copycat_protocol::Request {
        version: 99,
        id: "x".into(),
        action: Action::Status,
    };
    let error = copycat_protocol::request(&daemon.socket, &mismatched).unwrap_err();
    assert_eq!(error.code, "protocol_version");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn status_reports_the_clipboard_and_offset_zero_separately() {
    // R15 is intended behaviour, so `status` has to make it visible.
    let daemon = Daemon::start();
    daemon.copy("A");
    daemon.copy("B");
    daemon.ok(Action::PasteOffset { offset: 1, raw: false });

    let ResultBody::Status(status) = daemon.ok(Action::Status) else { panic!("expected status") };
    assert_eq!(status.os_clipboard.as_deref(), Some("A"));
    assert_eq!(status.core.latest.as_ref().unwrap().preview, "B");
}
