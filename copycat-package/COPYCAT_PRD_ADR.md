# Copycat — Product Requirements Document & Architecture Decision Record

**Status:** Finalized v1.0 baseline — amended 2026-09-01 (see §24)  
**Working product name:** Copycat  
**Selected brand direction:** Variant 4  
**Core language:** Rust  
**Initial platforms:** macOS, Windows, Linux  
**Primary interfaces:** daemon, CLI, TUI, programmable global bindings  
**Future GUI:** decoupled; evaluate GPUI first, Tauri 2 fallback

---

# 1. Product thesis

Most clipboard managers optimize the secondary problem: browsing clipboard history. Copycat is aimed at the primary action itself — **moving copied values from one place to another with deterministic ordering**.

A clipboard naturally forms a stream of copy events. Copycat turns that stream into explicit, programmable data-structure semantics:

- normal clipboard: latest value;
- stack: LIFO traversal;
- queue: FIFO traversal;
- group: aggregate several copied values and paste them as one payload;
- indexed paste: paste an older item directly;
- programmable bindings: keyboard actions invoke these semantics without opening a UI.

History search, pinning, visual browsing, and a polished desktop GUI are useful, but they are not the defining feature.

## Product principle

> The clipboard should behave exactly the way the user told it to behave.

The daemon is the product core. Every UI is a client of the same deterministic state machine.

---

# 2. Goals

## 2.1 Primary goals

1. Run continuously as a lightweight user daemon on macOS, Windows, and Linux.
2. Record clipboard copy events into a local history.
3. Preserve a raw event log while exposing deduplicated logical views when desired.
4. Implement deterministic stack, queue, group, and by-index paste operations.
5. Allow modes and operations to be invoked entirely from the keyboard.
6. Make all bindings and the leader key user-programmable.
7. Provide a CLI that exposes every core operation.
8. Provide a fast keyboard-first TUI using Ratatui.
9. Keep the core independent of any future desktop GUI framework.
10. Keep sensitive clipboard history local and encrypt persisted payloads at rest.

## 2.2 Secondary goals

- searchable history;
- pinning/favorites;
- per-content-type previews;
- session inspection;
- optional content retention policies;
- system tray/menu bar controls;
- polished native desktop UI later.

## 2.3 Non-goals for the first release

- cloud sync;
- team/shared clipboard;
- mobile clients;
- browser extension;
- AI classification;
- remote clipboard transport;
- automatic data transformation pipeline;
- replacing the OS clipboard subsystem itself.

---

# 3. User experience

## 3.1 Normal mode

With no special mode active, Copycat should be invisible.

The OS clipboard behaves normally: the last copied value is the current paste value. The daemon records copy events and maintains history.

The user may still invoke direct actions such as “paste the item one position before latest.”

## 3.2 Stack mode

The user activates stack mode. New external copy events become pushes. Each Copycat paste operation consumes the next logical stack item in LIFO order.

Consumption is **logical traversal**, not destructive deletion from history.

Example:

```text
copy A
copy B
copy C

stack next -> C
stack next -> B
stack next -> A
```

If `D` is copied while the active stack has already advanced to `B`, `D` becomes the next item:

```text
next -> D
then -> B
```

### Duplicate behavior

Normal stack start uses duplicate collapse. This addresses accidental repeated copy gestures.

A separate command starts a stack that preserves duplicates. That supports the intentional case where the user copied the same value twice because they want to paste it twice.

Important architecture rule: **both copy events remain in raw history either way**. Duplicate policy affects the active logical stack.

## 3.3 Queue mode

Queue mode is FIFO and therefore needs a start boundary.

Two creation forms are required.

### Form A — last N

The user requests a queue from the last N logical clipboard items.

For history:

```text
A, B, C, D, E   # chronological order
```

`queue --last 5` pastes:

```text
A -> B -> C -> D -> E
```

The queue is a snapshot. New copy events do not enter it.

### Form B — capture from now

The user starts an empty queue capture, copies an arbitrary number of items, then seals the queue.

```text
queue capture
copy A
copy B
copy C
queue seal
```

The queue size is three and pastes A, then B, then C.

## 3.4 Group mode

Two forms mirror queue behavior.

### Last N group

Paste the last N selected text entries as one value using a delimiter.

Default delimiter: newline.

```text
A
B
C
```

is pasted as one clipboard payload.

### Capture group from now

Start group capture, collect arbitrary copy events, then paste them as one aggregate.

Group order is chronological capture order.

## 3.5 Paste by position

The user can paste an older item directly without changing modes.

The core API should use explicit zero-based offsets from latest:

```text
offset 0 -> latest
offset 1 -> item before latest
offset 4 -> five items down
```

The UI may display friendly one-based labels but must convert them unambiguously.

Offsets index the **collapsed logical view** by default, not the raw log — see R2. `--raw` selects raw-log indexing.

## 3.6 Programmable leader and macros

Bindings are data, not hardcoded UI behavior.

There are two classes:

1. **direct hotkey** — one system-wide chord maps directly to an action;
2. **leader sequence** — a configurable leader activates a short sequence window, then subsequent key(s) resolve to an action.

The leader is fully configurable because common defaults can collide with tmux, window managers, IDEs, and terminal workflows.

Example conceptual mappings:

```text
leader + s      -> start deduplicated stack
leader + S      -> start duplicate-preserving stack
leader + q      -> begin queue capture
leader + g      -> begin group capture
leader + 2      -> paste offset 1
```

The command/action model must allow macros to pass arguments. UI layers should not contain one-off implementations of these operations.

---

# 4. Functional requirements

## 4.1 Clipboard capture

The daemon MUST:

- detect external clipboard changes;
- normalize captured representations into a platform-independent `ClipPayload`;
- compute a stable content hash over normalized representations;
- write a raw event record;
- update the hot in-memory history;
- persist according to retention/privacy settings;
- notify active stack/queue/group capture sessions.

The daemon MUST distinguish its own internal clipboard writes from external copy events.

### Initial content support

The data model MUST support multiple representations per clipboard event from the beginning.

Implementation priority:

1. plain text — v0.1 required;
2. HTML — v0.1/v0.2;
3. images — v0.2;
4. file lists — v0.2;
5. arbitrary/custom formats — later.

This prevents a text-only schema from becoming technical debt while keeping the first implementation focused.

## 4.2 Hot history

Maintain the most recent 100 entries in memory by default. This is configurable.

The hot history is optimized for:

- stack/queue creation;
- indexed paste;
- TUI display;
- common search.

Older persisted history remains queryable from storage.

## 4.3 Persistent history

Persistence is enabled by default but can be disabled.

Requirements:

- local only;
- encrypted payload bodies;
- configurable retention period;
- explicit clear/delete operations;
- no clipboard content in ordinary logs;
- schema migrations must be versioned.

## 4.4 Search

Search is useful but secondary.

First implementation:

- metadata filtering in SQLite;
- decrypt-and-scan candidate text payloads locally;
- hot-memory search for recent entries;
- a bounded persisted scan (R18) — search is not promised over unbounded history.

Do not create a plaintext FTS index that defeats payload encryption.

A later encrypted/tokenized search index may be introduced if performance requires it.

## 4.5 Paste execution

A Copycat paste action performs four distinct operations:

1. resolve the logical item or aggregate;
2. materialize/decrypt it;
3. write it to the actual system clipboard;
4. inject the normal platform paste chord into the currently focused application.

After successful write/injection, advance the active stack/queue cursor if the action is consuming.

The daemon's self-write MUST NOT become a new external history event.

Default paste chords:

- macOS: Command+V;
- Windows: Ctrl+V;
- Linux: Ctrl+V, configurable because terminal/application behavior can differ.

## 4.6 Session lifecycle

Modes are modeled as explicit sessions.

Each session contains:

- mode;
- state;
- ordered referenced clip IDs;
- cursor;
- duplicate policy;
- delimiter if applicable;
- created timestamp;
- capture boundary information.

Sessions are independent from history storage.

Initial policy: one active traversal/capture session at a time. The data model may allow multiple named sessions later.

---

# 5. Core domain model

## 5.1 `ClipEvent`

Conceptual shape:

```rust
struct ClipEvent {
    id: ClipId,
    captured_at: Timestamp,
    source: ClipSource,
    content_hash: ContentHash,
    formats: Vec<FormatDescriptor>,
    payload_ref: PayloadRef,
}
```

`source` distinguishes external activity from internal Copycat writes.

## 5.2 `ClipPayload`

```rust
struct ClipPayload {
    representations: Vec<Representation>,
}

struct Representation {
    media_type: String,
    bytes: Vec<u8>,
}
```

The exact type system can become richer, but the schema must not assume “clipboard = one string.”

## 5.3 `Session`

```rust
enum SessionMode {
    Stack,
    Queue,
    Group,
}

enum DuplicatePolicy {
    Collapse,
    Preserve,
}

struct Session {
    id: SessionId,
    mode: SessionMode,
    duplicate_policy: DuplicatePolicy,
    state: SessionState,
    item_ids: Vec<ClipId>,
    cursor: usize,
    delimiter: Option<String>,
}
```

## 5.4 Deduplication

Do not deduplicate the raw log destructively.

Create logical views by content hash and policy.

For consecutive copies:

```text
A A A B B C
```

raw history remains six events.

Collapsed view can become:

```text
A B C
```

Preserved view remains:

```text
A A A B B C
```

This also allows UI to display duplicate multiplicity later.

---

# 6. Architecture

## 6.1 Workspace layout

Recommended Rust workspace:

```text
copycat/
├── Cargo.toml
├── crates/
│   ├── copycat-core/        # pure domain/state machine — no OS, no db, no async
│   ├── copycat-protocol/    # IPC wire schema, shared by daemon and every client
│   ├── copycat-daemon/      # process, event coordination, service lifecycle
│   │   └── src/
│   │       ├── platform/    # clipboard, watcher, hotkey, leader, paste injection
│   │       └── store/       # SQLite + payload crypto + key management
│   ├── copycat-cli/         # CLI client
│   └── copycat-tui/         # Ratatui client
└── apps/
    └── copycat-gui/         # future; no core dependency on framework
```

`copycat-platform` and `copycat-store` are **modules inside the daemon**, not separate crates (ADR-011). Both have exactly one consumer and no test story independent of the daemon; publishing them as crates buys a manifest and a version number, not a boundary. The two boundaries that carry real weight are kept as crates: `copycat-core` stays pure so it is testable without an OS, and `copycat-protocol` is shared by clients that must not link SQLite, clipboard, or input code.

## 6.2 Dependency direction

```text
                        copycat-core
                     (pure: no OS, no db, no async)
                              ^
                              |
                       copycat-protocol
                     (wire types; core + serde only)
                     ^          ^          ^
                     |          |          |
            copycat-daemon  copycat-cli  copycat-tui
                     |
                     +-- platform/   (clipboard, input — daemon-internal)
                     +-- store/      (SQLite, crypto — daemon-internal)
```

No client crate depends on the daemon crate. A CLI binary must not transitively link SQLite or a clipboard backend.

`copycat-core` must remain usable in property tests with no OS, no database, and no async runtime.

## 6.3 Event coordination

The daemon can use Tokio for IPC, timers, storage orchestration, and asynchronous clients. Platform APIs that require dedicated event-loop threads must remain on those threads and communicate through channels.

Normalized daemon events:

```text
ClipboardChanged
HotkeyActivated
LeaderKeyObserved
ClientRequest
StorageCompleted
ShutdownRequested
```

The state machine responds with effects:

```text
PersistEvent
WriteClipboard
InjectPaste
AdvanceSession
BroadcastState
```

This effect boundary makes deterministic tests straightforward.

---

# 7. Platform abstraction

## 7.1 Traits

Conceptual adapter surface:

```rust
trait ClipboardBackend {
    fn read(&mut self) -> Result<ClipPayload>;
    fn write(&mut self, payload: &ClipPayload) -> Result<()>;
    fn available_formats(&mut self) -> Result<Vec<FormatDescriptor>>;
}

trait ClipboardWatcher {
    fn run(&mut self, tx: EventSender) -> Result<()>;
}

trait HotkeyBackend {
    fn register(&mut self, binding: DirectBinding) -> Result<()>;
}

trait LeaderBackend {
    fn arm(&mut self, timeout: Duration, tx: EventSender) -> Result<()>;
}

trait PasteInjector {
    fn paste(&mut self) -> Result<()>;
}
```

The concrete API can be async/channel-based where required, but these responsibilities should remain separate.

## 7.2 macOS

Requirements:

- clipboard read/write via NSPasteboard-backed library/API;
- clipboard change tracking by polling `NSPasteboard.changeCount` — macOS emits no pasteboard notification, so polling is the only mechanism, not a shortcut (ADR-014);
- global shortcut registration;
- tmux-style leader observation through a narrowly scoped event tap/input-monitor path;
- paste injection;
- menu bar later;
- launch agent installation for daemon startup.

Permissions must be detected and explained by `copycat doctor` rather than failing silently.

## 7.3 Windows

Requirements:

- clipboard listener through native window/message mechanism or library wrapper;
- global hotkeys;
- short-lived keyboard observation for leader sequences;
- SendInput-style paste injection;
- named-pipe/local-socket IPC;
- startup through normal per-user startup mechanism/service helper as appropriate.

## 7.4 Linux

Linux is explicitly split into X11 and Wayland paths.

### X11

- mature clipboard-manager access;
- direct global hotkey libraries are available;
- keyboard observation/injection can be implemented using X11/XInput mechanisms.

### Wayland

Clipboard managers depend on compositor-supported data-control protocols. `wl-clipboard-rs` is an appropriate low-level candidate when `ext-data-control`/`wlr-data-control` is available.

Global shortcuts should prefer XDG Desktop Portal GlobalShortcuts where supported.

**Important limitation:** a portable direct global shortcut is not the same as a tmux-style leader sequence. On Wayland, leader sequences may require desktop/compositor-specific support or a user-configured compositor shortcut that invokes Copycat actions. This must be surfaced as a capability difference, not hidden behind unreliable hacks.

The product still supports Linux fully through CLI/TUI and clipboard semantics; the exact system-wide leader experience can vary by compositor until the platform offers a universal mechanism.

---

# 8. IPC protocol

Use local-only IPC.

Recommended implementation: `interprocess` local sockets, giving Unix-domain sockets on Unix and named-pipe-backed local sockets on Windows.

Protocol v1 should favor inspectability over binary compactness:

- length-delimited JSON messages;
- explicit protocol version;
- request ID;
- typed action name;
- typed args;
- typed result/error;
- optional event subscription stream.

Example:

```json
{
  "version": 1,
  "id": "req-123",
  "action": "stack.start",
  "args": { "duplicates": "collapse" }
}
```

Do not expose TCP by default.

---

# 9. Storage and privacy

## 9.1 SQLite

Use SQLite through `rusqlite` with bundled SQLite for predictable packaging.

Suggested tables:

```text
clip_events
clip_payloads
clip_representations
sessions
pinned_items
schema_migrations
```

## 9.2 Encryption

Persistent clipboard payloads are sensitive.

Decision:

- generate a random 256-bit master key;
- store/retrieve it via the OS keyring using the Rust `keyring` crate;
- encrypt each payload independently with XChaCha20-Poly1305;
- generate a fresh 192-bit nonce per payload;
- store nonce + ciphertext + authentication tag in SQLite;
- keep ordinary metadata minimal and unencrypted only where needed for operation;
- never write plaintext payloads to logs.

Deletion should remove database rows. Secure physical erasure cannot be guaranteed on modern filesystems/SSDs and must not be promised.

## 9.3 Search privacy

No plaintext FTS index in v1.

Use hot-memory search and local decrypt/scan for persisted text. Optimize only after measuring real history sizes.

## 9.4 Pause and purge

Required commands:

- pause capture;
- resume capture;
- clear history;
- delete one item;
- clear only unpinned history;
- optional pause while screen/session is locked where platform APIs permit.

---

# 10. Self-write suppression

This is a critical correctness mechanism.

Every paste action writes an older payload back into the real OS clipboard. Clipboard watchers will then observe a change. Without suppression, Copycat would treat its own paste as a fresh copy event and corrupt stack/queue order.

Implementation strategy:

1. before internal write, record expected content hash plus a short-lived internal-write generation token;
2. perform write;
3. watcher receives change;
4. read/hash current clipboard;
5. if it matches the pending internal write in the valid window, classify it as internal and do not append an external event;
6. clear suppression state.

The implementation must handle platforms that emit more than one change notification for a multi-format write.

---

# 11. CLI

Use Clap derive API.

The CLI is a thin IPC client except for bootstrap/doctor operations that need local process inspection.

Core families:

```text
copycat daemon ...
copycat paste ...
copycat stack ...
copycat queue ...
copycat group ...
copycat history ...
copycat bind ...
copycat config ...
copycat tui
copycat doctor
```

See `docs/CLI_SPEC.md` for the command baseline.

---

# 12. TUI

Use Ratatui + Crossterm.

The TUI is not just a history picker. It is an operational console for the daemon.

## Required screens

### History

- newest-first list;
- type;
- preview;
- age/time;
- duplicate count indicator;
- pin indicator;
- search/filter;
- paste selected;
- delete selected.

### Active mode

- mode name;
- capture/traversal state;
- duplicate policy;
- cursor position;
- next item preview;
- queue/group size;
- start/stop/seal/reset actions.

### Bindings

- leader key;
- direct hotkeys;
- sequences;
- conflict/error state;
- reload config.

### Diagnostics

- platform backend;
- X11/Wayland;
- clipboard watcher health;
- permissions;
- keyring;
- storage path;
- daemon uptime;
- protocol version.

---

# 13. Future GUI decision

The GUI is intentionally postponed.

The desired direction is a high-control native developer-tool UI with command bars, action strips, compact popovers, and fast keyboard navigation.

## GPUI

GPUI is the framework used by Zed and is architecturally attractive: Rust-native, GPU accelerated, high control, strong developer-tool fit.

However, the current public framework remains pre-1.0 and its own getting-started documentation still warns about breaking changes. Copycat requires a reliable Windows story.

### Decision

Re-evaluate GPUI when GUI implementation begins. If current Windows support and API stability are acceptable, prefer GPUI.

## Tauri 2 fallback

Tauri 2 officially supports Windows, macOS, and Linux and keeps backend logic in Rust. It is the fallback if GPUI is not ready for the required cross-platform release.

The daemon/core architecture ensures switching this decision does not rewrite product behavior.

---

# 14. Configuration

Use TOML + Serde.

Configuration categories:

```text
history
privacy
defaults
leader
leader.bindings
hotkeys
platform.macos
platform.windows
platform.linux
ui.tui
```

Hot reload is desirable. `copycat bind reload` and SIGHUP on Unix should reload config; Windows gets an IPC reload action.

See `docs/config.example.toml`.

---

# 15. Testing strategy

## 15.1 Core property tests

The most important tests are platform-free.

Properties:

- collapsed stack never emits consecutive equal hashes from repeated copies;
- preserved stack preserves multiplicity;
- queue snapshot order is FIFO;
- queue capture order matches event order;
- stack new-copy push becomes next item;
- group preserves chronological order;
- failed paste effect does not advance cursor;
- internal writes do not create external events;
- history deletion does not silently mutate unrelated active sessions without an explicit policy.

Use table tests plus property testing for random event streams.

## 15.2 Adapter contract tests

Each platform implementation gets the same contract suite:

- write text -> read same text;
- external clipboard mutation -> watcher event;
- internal-write suppression path;
- direct hotkey registration;
- paste injection smoke test where CI environment permits;
- graceful error when permission/capability is absent.

## 15.3 Integration tests

Run daemon in a temporary profile with an in-memory/fake clipboard backend and local IPC endpoint.

Test CLI -> daemon -> core -> fake platform end-to-end.

## 15.4 Platform CI

GitHub Actions matrix:

- macOS arm64/x86_64 where available;
- Windows x86_64;
- Linux x86_64 X11-oriented automated path;
- separate Wayland integration jobs where compositor CI is practical.

Hardware/input-permission tests that cannot be reliable in hosted CI should have a small manual release checklist.

---

# 16. Performance targets

These are product targets, not premature microbenchmarks.

- daemon idle CPU: effectively negligible; polling backends should remain low duty cycle;
- direct action IPC round trip: < 10 ms local target before clipboard/input OS latency;
- hot-history indexed resolution: O(1) or O(log n), not database scan;
- active stack/queue next resolution: O(1);
- daemon memory with text-only hot history: comfortably below 50 MB baseline target;
- no GUI runtime in daemon process;
- startup to ready: < 250 ms target on a normal desktop, measured with the key already unlocked and the schema current. A first run, a keyring prompt, or a schema migration are explicitly outside this budget.

---

# 17. Observability

Use structured logs, but never clipboard bodies.

Safe fields:

```text
event_id
content_hash_prefix
payload_size
format_count
mode
session_id
backend
platform
latency_ms
error_kind
```

Optional metrics can be exposed through `copycat doctor --json` or a debug socket later. No telemetry/network reporting by default.

---

# 18. Packaging

## macOS

- signed/notarized application bundle when GUI/menu app exists;
- daemon/helper installed per user;
- launch-at-login option;
- Homebrew formula later.

## Windows

- signed installer/MSI or equivalent;
- per-user startup registration;
- optional WinGet package later.

## Linux

- tarball first;
- `.deb` and RPM packages;
- systemd user service where appropriate;
- desktop file/tray integration later;
- Wayland/X11 capability detection at runtime.

The CLI and daemon can ship before a GUI.

---

# 19. Delivery plan

## Phase 0 — pure core

Deliver:

- Rust workspace;
- `copycat-core`;
- event log model;
- stack/queue/group state machines;
- duplicate policies;
- property tests;
- fake clipboard backend.

No real OS integration yet.

## Phase 1 — daemon + text clipboard

Deliver:

- daemon event loop;
- text clipboard capture/read/write on macOS/Windows/Linux X11 first;
- self-write suppression;
- local IPC;
- CLI;
- direct indexed paste;
- stack mode;
- queue last-N;
- queue capture;
- group text aggregation.

## Phase 2 — platform keyboard layer

Deliver:

- direct global hotkeys;
- paste injection;
- permissions diagnostics;
- leader sequences on macOS/Windows/X11;
- Wayland direct-shortcut integration and documented fallback path.

## Phase 3 — persistence/privacy

Deliver:

- SQLite;
- encrypted payloads;
- OS keyring key;
- retention;
- search;
- pin/delete/clear;
- pause/resume.

## Phase 4 — TUI

Deliver:

- history screen;
- active mode screen;
- search;
- action execution;
- bindings screen;
- diagnostics.

## Phase 5 — richer clipboard formats

Deliver:

- HTML;
- image;
- file-list support;
- format-aware previews;
- max-size policies.

## Phase 6 — desktop GUI

Re-evaluate GPUI. If it has the required Windows support/stability, use it. Otherwise use Tauri 2. The GUI remains an IPC client of the daemon.

---

# 20. Acceptance criteria for v0.1

v0.1 ships **one reference platform working end to end**, plus the trait seams and the shared adapter contract suite (§15.2) that every further platform must pass. Additional platforms ship when they pass that suite; they do not gate v0.1 (ADR-015).

**Reference platform for the initial release: Linux/X11.**

## 20.1 Behavioural criteria

Verified against the fake backend in CI and re-verified by hand on the reference platform:

1. daemon starts, is reachable over its local socket, and shuts down cleanly;
2. external text copies are captured reliably;
3. default OS clipboard behavior remains normal;
4. user can paste latest, an explicit offset, or explicit clip ID;
5. stack can traverse LIFO;
6. stack default collapses duplicates;
7. stack can be started with duplicate preservation;
8. queue can be created from last N;
9. queue can capture from now and seal;
10. group can aggregate recent or newly captured text with delimiter;
11. daemon internal writes do not pollute history;
12. CLI can control every behavior;
13. config controls bindings and leader value;
14. core test suite proves ordering invariants;
15. no clipboard payload appears in logs;
16. the IPC endpoint rejects peers whose uid differs from the daemon's (§23.1);
17. `copycat doctor` names every unavailable capability rather than failing silently.

## 20.2 Per-platform promotion criteria

A platform is "supported" — not merely "compiles" — when it passes the full §15.2 adapter contract suite and a manual release checklist covering capture, self-write suppression, and paste injection. Until then `copycat doctor` reports it as experimental and the README says so.

TUI, persistence, encryption, and Wayland leader parity can follow in v0.2 if necessary, but the core architecture must already support them.

---

# ADR-001 — Rust core and daemon-first architecture

**Decision:** implement the product core, daemon, protocol, CLI, and TUI in Rust.

**Why:** the product is fundamentally a long-running systems utility with OS clipboard/input integration. Rust provides a good fit for cross-platform native adapters, deterministic core types, low idle overhead, and one implementation shared by CLI/TUI/future GUI.

**Consequence:** platform-specific APIs remain necessary. Rust does not eliminate OS differences; it gives one place to normalize them.

---

# ADR-002 — append-only raw history + logical mode views

**Decision:** preserve raw copy events and implement deduplication in logical mode/session views.

**Why:** accidental duplicate copies and intentional duplicate copies are indistinguishable if duplicates are destroyed during capture. Session-level policy preserves both use cases.

**Consequence:** storage may contain repeated content references. Deduplicate encrypted payload blobs by safe content-addressing only if it does not weaken privacy or complicate deletion semantics; this is optional optimization, not v0.1 behavior.

---

# ADR-003 — daemon owns state; all UIs are clients

**Decision:** only the daemon owns live clipboard state, mode sessions, persistence, and platform hooks.

**Why:** CLI, TUI, tray, and future GUI must never drift in behavior or race each other for clipboard ownership.

**Consequence:** local IPC is a first-class stable interface.

---

# ADR-004 — local socket IPC

**Decision:** use cross-platform local sockets (Unix domain socket / Windows named pipe abstraction) rather than localhost TCP.

**Why:** local sockets have clearer scope and avoid exposing a network service for a security-sensitive utility.

**Consequence:** protocol versioning is still required because clients and daemon may update independently.

---

# ADR-005 — Ratatui for the first interactive UI

**Decision:** use Ratatui for the TUI.

**Why:** keyboard-first UX matches the product, it is portable, and it lets the team build a useful operational interface before committing to desktop GUI framework tradeoffs.

**Consequence:** the first release can be highly functional without shipping a heavy graphical frontend.

---

# ADR-006 — defer GUI framework; GPUI preferred candidate, Tauri fallback

**Decision:** do not bind architecture to a GUI framework now.

**Why:** GPUI is highly aligned with the desired Zed-like control and Rust-native UI, but remains pre-1.0 and must be re-validated for Windows when GUI work begins. Tauri 2 has clear Linux/macOS/Windows support and is the conservative fallback.

**Consequence:** GUI communicates over the same IPC API as the TUI.

---

# ADR-007 — SQLite + application-layer payload encryption

**Decision:** store structured history metadata in SQLite and encrypt clipboard payloads using XChaCha20-Poly1305, with the master key stored in the OS keyring.

**Why:** clipboard history can contain credentials, source code, private messages, and customer data. Local storage should not be plaintext by default.

**Consequence:** full-text search cannot casually depend on plaintext SQLite FTS. Search must respect the encryption model.

---

# ADR-008 — Linux X11 and Wayland are separate capabilities

**Decision:** model Linux backends explicitly rather than claiming one uniform “Linux clipboard/hotkey” implementation.

**Why:** Wayland clipboard-manager access and global shortcuts depend on compositor protocols and portals. A tmux-like leader sequence is especially different from a single portal-registered shortcut.

**Consequence:** `copycat doctor` must report capabilities and fallbacks. The product should be deterministic about what is unavailable instead of silently failing.

---

# ADR-009 — working name only

**Decision:** continue using Copycat internally and for this design package, but do not treat the name as release-cleared.

**Why:** current public search shows multiple clipboard products named CopyCat/Copycat and a clipboard CLI named `copycat`.

**Consequence:** repo/package/binary naming should remain easy to rename until legal/product-name clearance is complete.

---

# ADR-010 — the IPC endpoint is access-controlled, not merely local

**Decision:** the daemon authenticates every IPC peer by OS-provided credentials and restricts the endpoint at creation time. Requirements in §23.1.

**Why:** ADR-004 established that local sockets avoid exposing a network service. That is necessary and not sufficient. The daemon hands out *decrypted* clipboard history over that socket, so the socket — not the SQLite file — is the real trust boundary. Encrypting payloads at rest while leaving an unguarded endpoint that reads them back in plaintext protects the disk and nothing else.

**Consequence:** socket creation is platform-specific beyond what `interprocess` abstracts: a mode-checked `0700` directory and `0600` socket plus `SO_PEERCRED` on Unix, an explicit user-only DACL plus `PIPE_REJECT_REMOTE_CLIENTS` on Windows. Peers running as the same uid remain trusted; that limit is deliberate and stated.

---

# ADR-011 — five crates, not seven

**Decision:** `copycat-platform` and `copycat-store` become modules of `copycat-daemon`.

**Why:** a crate boundary should buy compile-time enforcement of a dependency rule or an independent consumer. `core` (must stay OS-free) and `protocol` (must be linkable by clients that carry no SQLite or clipboard code) buy both. `platform` and `store` have one consumer each and are tested through the daemon either way.

**Consequence:** promoting either back to a crate later is a directory move and a manifest; the module boundary is already there. If a second consumer appears, promote it then.

---

# ADR-012 — offsets index the collapsed view

**Decision:** `--offset` resolves against the collapsed logical view (R1, R2); `--raw` opts into the raw log.

**Why:** the raw log exists to keep the truth of what happened (ADR-002). It is the wrong index for a human asking for "two items back", because a double-tapped copy silently consumes an offset. The append-only log and the default addressing scheme can and should differ.

**Consequence:** every offset-taking surface — CLI, bindings, TUI, future GUI — carries the same `raw` flag, and `copycat status` states which view an offset was resolved against.

---

# ADR-013 — keyring first, with a named degraded mode

**Decision:** obtain the master key from the OS keyring; fall back to a `0600` key file only when the keyring is unavailable and the fallback is not disabled; otherwise run in memory with persistence off. `doctor` always reports which mode is live (§23.2).

**Why:** ADR-007 mandated the keyring unconditionally. Minimal desktops, servers, containers, and CI routinely have no Secret Service, so unconditional means "does not run there". Silent fallback is worse: users would believe they have keyring-grade protection they do not have.

**Consequence:** three key-storage modes to implement and test, and a `doctor` output that has to be honest about degradation rather than green.

---

# ADR-014 — polling is the portable clipboard-watch baseline

**Decision:** the watcher interface is event-shaped, but the default implementation polls, and macOS polls by necessity.

**Why:** Windows offers clipboard-update messages and X11 offers selection events, but macOS exposes only `NSPasteboard.changeCount` — there is no notification to subscribe to. A design that assumes event delivery everywhere would have to grow a polling special case for macOS anyway.

**Consequence:** the idle-CPU target in §16 is a statement about poll interval and hash cost, not about being event-driven. The interval is configurable, defaults to 250 ms, and `doctor` reports the mechanism actually in use per platform.

---

# ADR-015 — v0.1 is one reference platform plus a contract suite

**Decision:** v0.1 requires Linux/X11 working end to end; macOS and Windows ship on passing the §15.2 contract suite.

**Why:** the draft required three platforms simultaneously, which makes v0.1 undeliverable and unverifiable in one increment, and it conflated "the adapter compiles" with "the platform works". The contract suite is the real gate, and it is per-platform by construction.

**Consequence:** the README and `doctor` must distinguish supported from experimental platforms. Cross-platform code is still written against the traits from day one — this changes the release gate, not the architecture.

---

# 21. Current technical baseline references

The architecture above was checked against current documentation on 2026-09-01. Detailed links are in `docs/RESEARCH_NOTES.md`.

Key current observations:

- Ratatui is actively maintained and suitable for the TUI.
- `arboard` provides cross-platform clipboard access and optional Wayland data-control support.
- `clipboard-rs` provides broad clipboard formats and watch support, useful as an implementation candidate but not something the core should depend on directly.
- `global-hotkey` supports Windows/macOS/X11, not general Wayland.
- Wayland has an XDG GlobalShortcuts portal for direct global shortcuts.
- GPUI is Zed's Rust GPU UI framework but remains pre-1.0.
- Tauri 2 officially targets Linux/macOS/Windows.
- `interprocess` provides cross-platform local sockets.
- current `keyring`, `rusqlite`, and RustCrypto crates support the selected local encrypted storage design.

---

# 22. Resolved semantics (normative)

Each item below was ambiguous or unstated in the draft and is now binding. Items marked *(tested)* must be covered by `copycat-core` tests that run without an OS, a database, or an async runtime.

## 22.1 Views and addressing

**R1 — Collapse is consecutive-only.** *(tested)*
A collapsed view drops an entry whose content hash equals that of the entry immediately preceding it chronologically. Non-adjacent repeats survive.

```text
raw        A A A B B C B
collapsed  A B C B
```

The problem being solved is the accidental repeated copy gesture, which is always adjacent. Global dedup would silently relocate "the thing I copied two minutes ago".

**R2 — `--offset` indexes the collapsed view; `--raw` indexes the log.** *(tested)*
Zero-based, newest first, `0 = latest`. After a double-tapped copy, `--offset 1` returning the text just copied is never the intent.

**R3 — `--last N` takes what exists.**
Fewer than `N` available entries is not an error; the operation uses all of them. Only an empty result is exit 7.

## 22.2 Sessions

**R4 — One active session; starting a mode replaces the previous one.**
`stack start`, `queue start`, `queue capture`, and `group capture` end any active session and return the replaced session's summary. Replacement is not an error: these are bound to a leader key, and a modal error requiring a second command to clear is hostile at that latency.

**R5 — Sessions are ephemeral.** They live in daemon memory and are dropped on restart. The `sessions` table is removed from §9.1.

**R6 — Exhausted traversal errors.** *(tested)*
`paste next` past the last item returns exit 7 `session_exhausted`. No wrap, no silent fallback to latest.

**R7 — A live stack push inserts at the cursor.** *(tested)*
An external copy during an active stack becomes the next item. Under `collapse`, a hash equal to the item currently at the cursor is not inserted — the raw event is still recorded.

**R8 — Capture collapse applies to the tail.** *(tested)*
During `queue capture` / `group capture` under `collapse`, a copy equal to the most recently captured item is not appended.

**R9 — Deletion removes the clip from the active session.** *(tested)*
The id is dropped from the session's item list; if its index was strictly below the cursor, the cursor decrements. This is the explicit policy required by STATE_MACHINE invariant 3.

**R10 — Session-referenced events are pinned in hot history.**
Eviction skips events an active session references, so the 100-item hot cap can never dangle a session.

## 22.3 Paste

**R11 — `paste next` with no active session** is exit 7 `no_active_session`.

**R12 — `--peek` never advances a cursor and never ends a session.**

**R13 — Group payloads are transient.** An aggregate is written to the OS clipboard and never recorded as a clip event under any source tag. It has no clip id and cannot be addressed by offset.

**R14 — Group skips non-text entries rather than failing.** Entries with no `text/plain` representation are ignored and the skipped count is reported. Exit 8 only when zero text entries remain.

## 22.4 Clipboard divergence

**R15 — After any Copycat paste, the OS clipboard and Copycat's `offset 0` deliberately differ.**

Copycat writes the resolved item to the OS clipboard and suppresses that write from history (§10). The OS clipboard therefore holds the pasted item while `offset 0` still resolves to the most recent *external* copy. A later manual Ctrl/Cmd+V pastes the Copycat-resolved item, not the user's last real copy.

This is inherent to operating through the system clipboard rather than a side channel. It must appear in user-facing help, and `copycat status` reports both values so the state is never a surprise.

## 22.5 Self-write suppression

**R16 — Suppression is `(hash, token, deadline)` and single-shot.**
Before an internal write the daemon records the expected content hash, a monotonic token, and a deadline (default 750 ms, configurable). The first observation inside the window whose hash matches is classified internal and consumes the record. Non-matching observations inside the window are external. The record clears on match or deadline. Platforms that emit several notifications for one multi-format write are handled by the single-shot rule plus hash equality.

**R17 — Accepted limitation.** An external copy byte-identical to a just-pasted item, inside the suppression window, is indistinguishable from the internal write and is dropped. Stated rather than hidden; the window is configurable for users who hit it.

## 22.6 Search

**R18 — Persisted search is bounded.** Search covers hot history plus the most recent `history.search_scan_limit` persisted payloads (default 2000), newest first, and sets `truncated: true` when the bound is reached. Decrypt-and-scan over unbounded history is not promised.

## 22.7 Configuration and capabilities

**R19 — Config version mismatch is fatal and explicit.** A `version` above the binary's supported version fails with exit 2, naming both versions. Lower versions are migrated in memory; the file is never rewritten without `copycat config migrate`.

**R20 — `privacy.pause_on_lock_screen` defaults to `false`** until a platform implementation exists, and `doctor` reports it as unimplemented rather than silently inert.

---

# 23. Security requirements

## 23.1 IPC access control

The daemon serves decrypted clipboard history over its local socket. That socket, not the database file, is the trust boundary (ADR-010).

Required:

- **Unix** — the socket lives in a directory owned by the daemon's uid with mode `0700`; the socket is created `0600`. Ownership and mode of both are verified at startup, and the daemon refuses to start if either is wrong.
- **Unix** — peer credentials (`SO_PEERCRED` / `LOCAL_PEERCRED`) are read for every connection and any peer whose uid differs from the daemon's is rejected and logged.
- **Windows** — the named pipe is created with an explicit DACL granting the creating user's SID only, and with `PIPE_REJECT_REMOTE_CLIENTS`.
- **All platforms** — no TCP listener exists in any build configuration, behind any flag.

Processes already running as the same user remain trusted. Defending a clipboard daemon against code running as its own user is out of scope, and saying so is better than implying protection that does not exist.

## 23.2 Key storage modes

The master key is obtained in this order:

1. **keyring** — OS keyring via the `keyring` crate. The default, and the only mode considered fully protected.
2. **key file** — used only when the keyring is unavailable *and* `privacy.allow_key_file_fallback` is true (default). A key file in the data directory, created `0600` in a `0700` directory, both verified.
3. **memory only** — keyring unavailable and fallback disabled. Persistence is off for the session; capture continues in hot history and nothing is written to disk.

`doctor` always names the live mode and marks mode 2 as degraded. Mode 2 logs exactly one warning at startup.

## 23.3 Payload handling

Unchanged from §9.2 and §17, restated as requirements: payload bytes never enter logs at any level; error messages carry `content_hash` prefixes and sizes, never content; `doctor --json` carries no payloads.

---

# 24. Amendment log

Applied 2026-09-01 to the draft baseline, in response to a pre-implementation review.

| # | Change | Sections |
|---|---|---|
| 1 | Twenty ambiguities resolved as normative rules R1–R20 | §22 |
| 2 | IPC access control specified; identified as the actual trust boundary | §23.1, ADR-010 |
| 3 | Key storage given three named modes instead of an unconditional keyring requirement | §23.2, ADR-013 |
| 4 | Workspace reduced from seven crates to five | §6.1, §6.2, ADR-011 |
| 5 | Offsets defined against the collapsed view, with `--raw` opt-out | §3.5, ADR-012 |
| 6 | Clipboard/`offset 0` divergence documented as intended behaviour | R15 |
| 7 | Self-write suppression given a precise rule and a stated failure case | R16, R17 |
| 8 | Search bounded and required to report truncation | §4.4, R18 |
| 9 | v0.1 retargeted to one reference platform plus a contract suite | §20, ADR-015 |
| 10 | Clipboard watching acknowledged as polling-based, macOS necessarily so | §7.2, ADR-014 |
| 11 | Startup budget qualified; `pause_on_lock_screen` defaulted off | §16, R20 |
| 12 | Sessions declared ephemeral; `sessions` table removed | R5 |

Open items deliberately left unresolved: the product name (ADR-009), the GUI framework (ADR-006), and Wayland leader-sequence parity (ADR-008). Each is blocked on information that does not exist yet, and inventing an answer now would be worse than carrying the decision.
