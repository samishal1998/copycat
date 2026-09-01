# Technical Research Notes

Verified on 2026-09-01 against current project documentation.

## Clipboard access

- `arboard` 3.6.1 provides cross-platform text/image clipboard access on Linux, macOS, and Windows. Its Linux backend can use Wayland data-control with the relevant feature, otherwise X11/XWayland.
  - https://docs.rs/arboard/latest/arboard/
- `clipboard-rs` 0.3.4 provides text, HTML, RTF, image, file list, custom formats, and clipboard watching across Windows/macOS/Linux, but its Linux support table is X11-oriented.
  - https://docs.rs/clipboard-rs/latest/clipboard_rs/
  - https://github.com/ChurchTao/clipboard-rs
- `wl-clipboard-rs` is designed for terminal/clipboard-manager utilities on Wayland using `ext-data-control` or `wlr-data-control` when supported by the compositor.
  - https://github.com/YaLTeR/wl-clipboard-rs

**Decision:** hide all clipboard access behind `ClipboardBackend`. Use the strongest common library first, but keep explicit Linux X11/Wayland adapters because Linux clipboard-manager behavior is not uniform enough to let a single crate become an architectural dependency.

## Clipboard monitoring

`clipboard-master` demonstrates the practical platform split: Windows can receive clipboard update messages; macOS generally watches `NSPasteboard.changeCount`; X11 uses selection mechanisms.

- https://docs.rs/clipboard-master/latest/clipboard_master/

**Decision:** monitoring is an adapter capability. The core receives normalized `ClipboardChanged` events and does not know whether the platform used events or polling.

## Global shortcuts and leader sequences

`global-hotkey` 0.8.0 supports Windows, macOS, and Linux X11, with platform event-loop constraints. It does **not** solve Wayland globally.

- https://docs.rs/global-hotkey/latest/global_hotkey/

Wayland has an XDG Desktop Portal GlobalShortcuts interface that can register shortcuts, but a tmux-style “leader, then arbitrary next key” is not equivalent to one registered shortcut.

- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html

**Decision:** support two binding classes:

1. direct global shortcuts;
2. tmux-style leader sequences.

Leader sequences require a short-lived platform key-observation path after leader activation. On Wayland, the first release may need compositor/desktop integration or direct shortcuts as a fallback where a safe portable observation mechanism is unavailable.

## TUI

Ratatui 0.30.2 is the selected TUI framework.

- https://docs.rs/ratatui/latest/ratatui/

## Future GUI

GPUI is Zed's GPU-accelerated Rust UI framework. The current crate is 0.2.x and its own README still warns that it is pre-1.0 and undergoing breaking changes; its getting-started documentation has historically focused on macOS/Linux. It is attractive for the desired native/developer-tool feel, but Copycat must ship Windows too.

- https://gpui.rs/
- https://github.com/zed-industries/zed/tree/main/crates/gpui

Tauri 2 officially targets Linux, macOS, and Windows and remains the fallback if a stable Windows-capable native Rust GUI choice is not ready when GUI work starts.

- https://v2.tauri.app/start/
- https://v2.tauri.app/start/prerequisites/

**Decision:** no GUI framework is coupled to the core. Re-evaluate GPUI first when GUI work begins; use Tauri 2 if Windows support or framework stability blocks GPUI.

## IPC

`interprocess` local sockets provide a cross-platform abstraction over Unix-domain sockets and Windows named-pipe based local sockets.

- https://docs.rs/interprocess/latest/interprocess/local_socket/

## Storage and encryption

- `rusqlite` 0.40.x supports a bundled SQLite build, useful for predictable desktop packaging.
  - https://docs.rs/rusqlite/latest/rusqlite/
- `keyring` 4.2.x exposes native key stores including Apple and Windows stores and Linux options.
  - https://docs.rs/keyring/latest/keyring/
- RustCrypto `chacha20poly1305` 0.11.x includes `XChaCha20Poly1305`.
  - https://docs.rs/chacha20poly1305/latest/chacha20poly1305/

**Decision:** SQLite stores metadata and encrypted payload blobs. A random master key is stored through the OS keyring. Payload encryption uses XChaCha20-Poly1305 with a fresh nonce per payload. Search should not silently introduce a plaintext full-text index.

## Naming

The working name **Copycat** has substantial collision risk. Current public products include clipboard managers named CopyCat/Copycat and a cross-platform clipboard CLI named `copycat`.

This is not a legal trademark conclusion; it is enough to require product-name clearance before public packaging.
