# Copycat

A programmable clipboard daemon. The clipboard is a stream of copy events, and
Copycat gives that stream explicit semantics: stack, queue, group, and paste by
position, driven from the keyboard.

Design, decisions, and the resolved semantics live in
[`copycat-package/COPYCAT_PRD_ADR.md`](copycat-package/COPYCAT_PRD_ADR.md).
The rules referenced below as **R1**–**R20** are section 22 of that document.

> **Working name.** Existing clipboard products already use "Copycat"
> (ADR-009). The crate, binary, and socket names are kept easy to change.

## Try it

Nothing here needs a display — the file-backed clipboard exists exactly so the
daemon can be driven on a machine without one.

```sh
cargo build --release

export PATH="$PWD/target/release:$PATH"
copycat daemon start -- --clipboard file --clipboard-file /tmp/cb.txt

printf alpha > /tmp/cb.txt; sleep 0.3   # another application copies
printf beta  > /tmp/cb.txt; sleep 0.3
printf gamma > /tmp/cb.txt; sleep 0.3

copycat stack start
copycat paste next        # gamma
copycat paste next        # beta
copycat status            # the clipboard and offset 0 differ on purpose (R15)
copycat tui               # the operational console
copycat doctor            # what works here, and what does not
```

On a desktop, drop `--clipboard file …` and it uses the system clipboard.

## Layout

Five crates, and the two boundaries that carry weight are the two that are
enforced (ADR-011):

```text
copycat-core        the state machine — no OS, no database, no async runtime
copycat-protocol    wire schema, framing, and the client transport
copycat-daemon      event loop, platform adapters, encrypted store   (copycatd)
copycat-cli         the command line                                 (copycat)
copycat-tui         the terminal console
```

`copycat-core` stays pure so the ordering rules that define the product can be
tested exhaustively without a clipboard. `copycat-protocol` is what clients link
instead of the daemon, so a CLI invocation carries no SQLite and no clipboard
backend. Everything else is a module.

Only the daemon owns clipboard state (ADR-003). The CLI, the TUI, and a key
binding are the same client making the same request, which is why they cannot
disagree about what "next" means.

## What works

| | |
|---|---|
| Capture, hot history, encrypted persistence | yes |
| Stack, queue (snapshot and capture), group | yes |
| Paste by latest, offset, id, or session | yes |
| Duplicate collapse and preserve | yes — see the caveat below |
| Self-write suppression | yes |
| CLI, TUI, `doctor`, JSON output | yes |
| Global hotkeys | implemented, unverified without a display |
| Leader sequences | X11 only, by construction (ADR-008) |

**Platform support** is a claim about verification, not compilation
(ADR-015). Everything below compiles; `doctor` reports the difference at
runtime.

| Platform | State |
|---|---|
| Linux headless | **the only configuration actually exercised** — CLI, IPC, sessions, history, and persistence all run and are covered by the test suite; capture and injection have nothing to talk to |
| Linux / X11 | the intended reference platform, **not yet exercised**: the XTEST injector, the keyboard-grab leader, hotkey registration, and the `arboard` backend are compile-checked only |
| Wayland | experimental — clipboard through XWayland, no leader sequences by construction |
| macOS, Windows | written against the documented APIs, **not yet run** |

Nothing with a display has been run yet: this was built on a headless machine.
The §15.2 adapter contract suite that §20.2 makes the promotion gate does not
exist as a named artifact, so no platform has passed it. Treat every row above
the headless one as "written, plausible, unproven".

### Caveats worth knowing before you rely on them

**Repeat copies need a change counter.** Detecting that the same value was
copied twice requires a change token from the platform — `NSPasteboard.changeCount`,
the Windows clipboard listener, X11 XFixes. `arboard` exposes none, so on X11 a
repeat copy is currently invisible and `--duplicates preserve` has nothing to
preserve. The trait and the watcher already handle it; the X11 backend does not
yet supply it. `doctor` reports this as degraded rather than letting the
duplicate policy quietly do nothing.

**Text only, for now.** `arboard` can write HTML but not read it, so claiming
HTML capture would mean recording something the user never copied. The data
model carries multiple representations already, so this is a backend change
rather than a schema change.

**The Linux paste chord is not configurable.** §4.5 requires it to be, because
terminals differ (Ctrl+Shift+V). The chord is currently compiled in, and §14's
`platform.linux` / `platform.macos` / `platform.windows` config tables do not
exist.

**Some config keys are parsed but unused.** `ui.tui.preview_lines` and
`ui.tui.show_duplicate_runs` are validated and then ignored. Reloading config
rebuilds bindings only — a changed `hot_items` or `watch_interval_ms` needs a
restart.

**The Windows IPC ACL is not implemented.** Section 23.1 requires a user-only
DACL and `PIPE_REJECT_REMOTE_CLIENTS`; `interprocess` exposes neither. On Unix
the peer uid is checked on every connection and the socket is `0600` inside a
verified `0700` directory. `doctor` reports the Windows gap as degraded.

## Security

The socket, not the database file, is the trust boundary (ADR-010): the daemon
serves *decrypted* history over it. Encrypting payloads at rest while leaving
the endpoint open would protect the disk and nothing else.

- Payloads are sealed with XChaCha20-Poly1305, a fresh 192-bit nonce each.
- The key comes from the OS keyring; failing that, and only if the config
  permits it, a `0600` key file; failing that, the daemon runs without
  persistence. `doctor` always names which (ADR-013).
- Payload bytes never reach a log at any level. Logs carry ids, hash prefixes,
  sizes, and error kinds.
- Processes running as the same user are trusted. That is a stated limit, not
  an oversight: a clipboard daemon cannot defend against code already running
  as its own user.

## Tests

```sh
cargo test --workspace
```

157 tests. The ones that matter:

- **Ordering invariants** (`copycat-core/tests/ordering.rs`) — section 15.1 plus
  every R-rule that touches ordering, driven through the full paste handshake
  rather than by calling `commit_paste` directly. Two randomized event-stream
  properties over 200 seeds each check that a collapsed stack never emits
  adjacent repeats, a preserved stack is exactly the reversed log, and
  interleaved copies and pastes never lose or duplicate an item.
- **Integration** (`copycat-daemon/tests/end_to_end.rs`) — 16 tests against a
  real daemon over a real socket, covering the watcher, framing, persistence,
  and the paste transaction as one path. They poll for state instead of
  sleeping, so a slow machine is slow rather than flaky.

## Configuration

`docs/config.example.toml` in the package directory is the annotated reference,
and is parsed by the test suite so it cannot drift from the schema. A config
written for a newer version fails by version rather than by whichever unknown
key came first (R19).
