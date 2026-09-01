# Copycat CLI & Command Model

The CLI is both a user tool and the reference interface to the daemon protocol. GUI/TUI/hotkey layers should call the same actions rather than reimplement clipboard semantics.

## Process control

```text
copycat daemon start
copycat daemon stop
copycat daemon restart
copycat daemon status
copycat doctor
copycat tui
```

`doctor` checks clipboard backend, keyboard permissions, IPC socket/pipe, storage access, keyring access, Wayland/X11 state, and registered shortcuts.

## Basic paste

```text
copycat paste latest
copycat paste --offset 1       # one item before latest
copycat paste --offset 4       # five items down if latest is offset 0
copycat paste --offset 1 --raw # index the raw log instead of the collapsed view
copycat paste --id <clip-id>
copycat paste --peek           # select/write/paste without advancing active mode
```

Offsets are zero-based internally. The UI may display “1 = latest” for humans, but the CLI keeps explicit `--offset` semantics to avoid ambiguity.

`--offset` indexes the **collapsed** logical view by default (R1, R2). `--raw` indexes the raw append-only log. `--peek` never advances a cursor and never ends a session (R12).

After any paste, the OS clipboard holds the pasted item while `--offset 0` still resolves to the last *external* copy. This divergence is intended (R15); `copycat status` prints both.

## Stack mode

```text
copycat stack start
copycat stack start --duplicates preserve
copycat stack start --duplicates collapse
copycat stack stop
copycat stack status
copycat stack reset
copycat paste next
```

Starting any mode replaces the currently active session and reports what it replaced; that is not an error (R4). `paste next` with no active session is exit 7 `no_active_session` (R11); past the last item it is exit 7 `session_exhausted` — it does not wrap (R6).

Default stack duplicate policy: `collapse`.

Semantics:

- copy events always enter the raw history;
- active stack view uses the selected duplicate policy;
- external copies push onto the active stack;
- `paste next` resolves the current top item, writes it to the system clipboard, injects the platform paste chord, and advances the logical stack only after the write/injection path succeeds;
- advancing does **not** delete history.

## Queue mode

### Snapshot the last N items

```text
copycat queue start --last 5
copycat queue start --last 5 --duplicates preserve
copycat paste next
```

For `--last 5`, Copycat snapshots the selected five logical items and pastes from oldest to newest.

### Capture from now

```text
copycat queue capture
# user copies items
copycat queue seal
copycat paste next
copycat queue stop
```

`queue capture` starts an empty queue. Every subsequent external copy is appended. `queue seal` freezes capture and makes the first captured item the next item to paste. The final queue size is therefore whatever was captured between `capture` and `seal`.

## Group mode

### Group last N

```text
copycat group paste --last 5
copycat group paste --last 5 --delimiter '\n'
copycat group paste --last 2 --delimiter ', '
```

### Capture a group from now

```text
copycat group capture --delimiter '\n'
# user copies arbitrary items
copycat group paste
copycat group end
```

Group mode aggregates selected textual representations into one payload. Non-text formats are rejected in the first implementation unless a mode-specific serializer is explicitly configured later.

## History

```text
copycat history list --limit 100
copycat history show <id>
copycat history search 'postgres'
copycat history delete <id>
copycat history clear
copycat history pause
copycat history resume
copycat history list --raw          # raw log instead of the collapsed view
```

`history search` scans hot history plus a bounded window of persisted payloads and sets `truncated` when the bound is hit (R18).

## Binding and leader commands

```text
copycat bind list
copycat bind reload
copycat bind test
copycat config show
copycat config path
copycat config migrate
copycat status
```

`copycat status` reports the active session, the daemon's `offset 0`, the value currently on the OS clipboard, key-storage mode, and platform capabilities.

Bindings live in TOML. They resolve to named daemon actions.

## Exit codes

- `0`: success
- `2`: invalid CLI/config
- `3`: daemon unavailable
- `4`: clipboard backend unavailable
- `5`: shortcut/input permission unavailable
- `6`: storage/keyring unavailable
- `7`: requested clip/session not found
- `8`: unsupported content type for action
- `9`: platform feature unavailable
