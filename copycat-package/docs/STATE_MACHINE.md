# Copycat Core State Model

## Fundamental model

The product should be implemented as an **append-only clipboard event log** plus mode-specific logical views. A copy event and a paste traversal are separate concepts.

This distinction solves the duplicate problem cleanly: the raw history can preserve what actually happened while a stack/queue/group session can either collapse or preserve consecutive duplicate payloads.

## Core entities

```rust
ClipEvent {
    id,
    captured_at,
    source,              // external | internal
    content_hash,
    formats,
    payload_ref,
}

Session {
    id,
    mode,                // stack | queue | group
    state,
    duplicate_policy,    // collapse | preserve
    item_ids,
    cursor,
    delimiter,
}
```

The actual Rust types should remain in `copycat-core` and contain no OS dependencies.

## Invariants

1. External clipboard changes append raw events.
2. Copycat's own clipboard writes are tagged/suppressed so they do not recursively become new external history entries.
3. History deletion and session consumption are different operations.
4. `paste next` never deletes the underlying event.
5. Duplicate collapse is a **view/session policy**, not destructive history mutation.
6. Queue snapshots do not change when new copies occur unless that queue is explicitly in capture state.
7. Stack sessions can receive new external copies at the top while active.
8. Group capture preserves capture order.
9. Mode state is deterministic and testable without a real clipboard.
10. Clipboard I/O, paste injection, storage, and hotkeys are adapters around the core state machine.

## Default mode

No traversal session is required. The daemon records external clipboard changes and normal paste semantics remain “latest item.” Copycat commands can still paste by offset or ID.

## Stack

Initial state:

```text
latest -> A B C D
stack  -> A B C D
next   -> A
```

After `paste next`:

```text
stack cursor -> B
```

If a new external item `X` is copied while the stack is active:

```text
next -> X
then -> B
```

This behaves like a logical push onto the active unconsumed stack.

### Duplicates

Raw events:

```text
A A B
```

With `collapse`:

```text
A B
```

With `preserve`:

```text
A A B
```

## Queue — last N

History newest-first:

```text
E D C B A
```

`queue start --last 5` produces:

```text
A B C D E
^ next
```

New copies do not enter this snapshot queue.

## Queue — capture from now

```text
queue capture
copy A
copy B
copy C
queue seal
```

Result:

```text
A B C
^ next
```

## Group

`group paste --last 3 --delimiter '\n'` resolves the selected entries in chronological order and creates one transient payload:

```text
A\nB\nC
```

The transient group payload may be written to the system clipboard but should not be re-captured as a new external event.

## Paste transaction

A paste action is:

```text
resolve logical item(s)
  -> materialize/decrypt payload
  -> write selected payload to system clipboard
  -> mark self-write suppression token/fingerprint
  -> inject native paste keystroke
  -> on success, advance session cursor
  -> emit PasteCompleted event
```

If clipboard write fails, do not advance. If paste injection fails, default to not advancing; expose a config option only if real-world testing shows a need for alternate behavior.
