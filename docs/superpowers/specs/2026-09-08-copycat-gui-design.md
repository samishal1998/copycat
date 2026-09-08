# Copycat desktop GUI — design

**Status:** approved design, pre-implementation
**Date:** 2026-09-08
**Decision it acts on:** ADR-006 (GUI deferred; re-evaluate GPUI vs Tauri when GUI work begins)

## 1. What this is

A desktop GUI for Copycat with two surfaces:

- a **menu-bar app** — a tray icon always present; click for the current mode, recent history, and one-key session controls;
- a **window** — the four screens the TUI has (History, Session, Bindings, Diagnostics), as a real GUI.

It is built menu-bar first, window second, as two slices.

## 2. Core principle — another daemon client

The daemon owns all state and speaks length-delimited JSON over a local socket
(ADR-003); `copycat-protocol` already carries the client transport
(`call`, `default_socket_path`), the typed `Action` requests, and the
`ResultBody` responses. **The GUI is one more client of that socket.** No daemon
changes are required for v1; every button maps to an action the daemon already
has (`paste.id`, `paste.mode`, `stack.start`, `history.list`, `bind.set`,
`doctor`, …). This is the whole payoff of the daemon-first architecture, and the
reason a GUI is a small subsystem rather than a rewrite.

## 3. Framework — Tauri 2

Chosen over GPUI and a native app because it is the only option that gives both
surfaces (built-in system tray **and** a window), cross-platform, on a stable
base, with the daemon-facing logic in Rust. GPUI has no real tray story and is
pre-1.0; a native Swift app drops cross-platform and Rust. (GPUI was ADR-006's
first preference; the menu-bar-first requirement is what tips it to Tauri, which
ADR-006 named as the fallback.)

Frontend: **vanilla HTML/CSS/JS**, no build step — smallest thing that works,
easiest to keep correct and for the user to tweak.

## 4. Repository shape

```
apps/copycat-gui/
├── Cargo.toml          # its OWN workspace, NOT a member of the root workspace
├── src-tauri/          # the Tauri Rust shell; depends on copycat-protocol by path
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/main.rs
└── ui/                 # index.html, style.css, app.js  (the two surfaces)
```

**`apps/copycat-gui` is a separate cargo workspace, excluded from the root
`[workspace]`.** Reason: Tauri's Linux build pulls `webkit2gtk`, `libsoup`,
`gtk`, which are not installed on the dev box and must not enter the root
build — keeping them out means `cargo test --workspace` and the existing CI stay
green and fast. The GUI has its own build and its own CI job. The Rust shell
depends on `copycat-protocol` by relative path, reusing the transport and types
rather than reimplementing them.

## 5. Data flow

- The frontend calls Tauri **commands** (`invoke`) that wrap
  `copycat_protocol::call(socket, action)` and return the `ResultBody` as JSON.
- A background task in the Rust shell **polls** `status` + `history.list` every
  ~500 ms and emits a Tauri **event**; the frontend listens and re-renders the
  mode indicator and history. Polling, not a subscription — it is what the TUI
  does, and a real event stream on the protocol is a later optimization, not a
  v1 blocker.
- Pasting an item is `paste.id`; the mode paste is `paste.mode`. On macOS these
  inject through the daemon's existing path (needs Accessibility, already
  handled). The socket is resolved with `copycat_protocol::default_socket_path`,
  so the GUI and CLI always agree on where the daemon listens.

## 6. The two surfaces (from the approved mockup)

Design tokens, fixed by the mockup:

- **Color:** charcoal surfaces (`#1C1F23`, raised `#23272E`), burnt orange
  `#E4672B` for *active semantic state only* (the mode, the cursor, the next
  item), soft gold `#D2A84A` for pins, slate for the inactive. Single dark
  world by choice.
- **Type:** Bricolage Grotesque (display), Inter (UI prose), **JetBrains Mono
  for all data** — clip contents, ids, counts, keycaps.
- **Motion:** interruptible transitions naming exact properties; `scale(0.96)`
  on press; ≤120 ms hover cues; every state change also carries a color/label
  cue; `prefers-reduced-motion` disables it.

**Menu bar (slice 1):** header (mark, connection dot, chord hint); a mode strip
showing the active session and its next item as one highlighted line; the recent
history as a list (click to paste, age/pin/type markers); a footer of one-key
actions (paste, stack, queue, group, settings). Left-click opens the panel;
right-click a small native menu (Open window, Pause, Quit).

**Window (slice 2):** a left rail (History / Session / Bindings / Diagnostics,
daemon status, version) and a content pane per screen. The **Session** screen is
the identity: the active session as a plain vertical list with the next item
marked (orange edge, `next ⌘V`), a duplicates toggle, and Pop / Reset / Stop
controls. History is a searchable list with previews and pin/delete/paste.
Bindings reuses the selector form already built for the TUI. Diagnostics renders
`doctor`.

## 7. Verification — honest about the constraint

**No GUI framework compiles on the dev box** (headless Linux; `webkit2gtk` et al.
absent, unable to install). So, exactly like the daemon's platform adapters:

- the Rust shell is written against the Tauri 2 docs and **compiled by CI** —
  macOS is the primary target and the one confirmed platform;
- there is little GUI-specific Rust *logic* to unit-test (the shell is thin glue
  over `copycat-protocol`, which is already tested); what exists is checked in
  the GUI CI job;
- the frontend and the real look are **verified by the user on macOS**;
- iteration on the shell is CI-driven and therefore slower than the daemon work
  was — accepted, and the reason to keep the shell as thin as possible.

## 8. Packaging & release

Tauri produces a `.app`/`.dmg` on macOS. The GUI ships through a **GUI release
job** building the macOS bundle, on the same pre-release discipline as the rest:
Windows/Linux GUI bundles are held until confirmed on a display, and anything
touching an unverified platform path goes out as `-rc`. The CLI/daemon release
workflow is unchanged.

## 9. Scope — what v1 is and is not

**Slice 1 (menu bar):** mode indicator, recent history with click-to-paste,
start/seal/stop of stack/queue/group, pause/resume. The daily driver.

**Slice 2 (window):** the four screens.

**Not in v1:** a protocol event-subscription stream (poll instead); Windows and
Linux GUI bundles (held); image/rich-format previews (text previews only, as
today); auto-launch/login-item integration (later).

## 10. Open items carried, not resolved

- Whether polling at 500 ms feels live enough on the menu bar, or the protocol
  needs a real subscription — measured after slice 1, not guessed now.
- Login-item / launch-at-startup for the GUI — a packaging concern deferred with
  the rest of §18 of the PRD.
