# copycat-gpui

Copycat's desktop window drawn with [GPUI](https://www.gpui.rs) and GPUI Kit
instead of a webview. Like every other interface it is only a client: all
state lives in `copycatd`, and this app asks for it over the local socket.

The Tauri app in `apps/copycat-gui` is the reference for behaviour. This one
covers its main window and, on macOS, its menu-bar dropdown:

- **History**: search, the clip list (preview, age, pin marker), click a row to
  paste it, pin and unpin, delete one, clear unpinned or clear all. On macOS a paste hides
  the window first so the chord lands in the app you were in.
- **Session**: the active mode and its details, the items in paste order with
  up and down buttons to reorder them, and the eight session controls.
- **Settings**: the `doctor` diagnostics, and when the keyboard tap was refused,
  a permission callout with the macOS panes to open and a daemon restart.
- **Sidebar**: the mode chip, the connection state, the daemon version, and the
  permission alert.
- **Menu bar (macOS)**: a template icon. Left click toggles a dropdown under
  the icon, on the screen you clicked, that closes when it loses focus. It shows
  the connection dot, a button that opens the full window, the mode box (mode,
  count, and the clip the next ⌘V pastes), a paged history (− n + rows per page,
  ◀ ▶ to page, click a row to paste it), and stack, queue, group and
  pause/resume buttons. Right click offers Open Copycat and Quit Copycat. The
  app keeps running in the menu bar with the main window closed.

On launch it starts the daemon (or takes over an incompatible one) through
`copycat-launcher`, the same code the Tauri app uses. It then polls `status`
every 600ms, one poll shared by the main window and the dropdown. It fetches
`doctor` on connect, when a session starts, and about every 10s, and refetches
history when `status` shows a new copy or a changed count.

Untested on a Mac so far: whether the dropdown's paste lands in the app you
were in, whether it opens over fullscreen apps without switching Spaces, and
whether a second click on the icon closes it cleanly.

## Not here yet

- Tray on Linux and Windows
- Floating button
- Debug overlay
- Bindings editor
- Drag-to-reorder (the up and down buttons cover reordering)
- Bundling and signing
- Linux and Windows builds

## Building

```sh
cargo run --manifest-path apps/copycat-gpui/Cargo.toml
```

This app is a separate cargo workspace, outside the root build. GPUI's Linux
backend needs Vulkan, Wayland and xkbcommon headers, and keeping the app apart
keeps those headers out of the root build. CI builds it on macOS
(`.github/workflows/gpui.yml`).
