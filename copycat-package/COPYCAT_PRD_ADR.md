# Copycat — Product Requirements Document & Architecture Decision Record

**Status:** Draft / implementation-ready baseline  
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
- hot-memory search for recent entries.

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
│   ├── copycat-core/        # pure domain/state machine
│   ├── copycat-protocol/    # IPC request/response/event schema
│   ├── copycat-platform/    # clipboard, hotkey, key-observation, paste injection traits
│   ├── copycat-store/       # SQLite + crypto/keyring
│   ├── copycat-daemon/      # process, event coordination, service lifecycle
│   ├── copycat-cli/         # CLI client
│   └── copycat-tui/         # Ratatui client
└── apps/
    └── copycat-gui/         # future; no core dependency on framework
```

## 6.2 Dependency direction

```text
copycat-core
   ^
   |
copycat-protocol       copycat-platform       copycat-store
   ^                         ^                     ^
   |_________________________|_____________________|
                             |
                      copycat-daemon
                       ^           ^
                       |           |
                copycat-cli   copycat-tui
```

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
- clipboard change tracking via pasteboard change count or library wrapper;
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
- startup to ready: < 250 ms target on a normal desktop.

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

A build is a meaningful v0.1 when all of the following are true:

1. daemon runs on macOS, Windows, and at least X11 Linux;
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
15. no clipboard payload appears in logs.

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
