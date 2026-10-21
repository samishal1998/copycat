# Guide: daemon + Tauri 2 GUI (tray dropdown, floating button, main window)

Audience: a coding agent building a NEW app shaped like Copycat. "Reusable" = copy the pattern. "Copycat-specific" = names/actions you replace. All paths are in the Copycat repo (branch `main`) unless stated. Tauri 2.12 / tao 0.37.1 / tray-icon 0.25.1 / objc2 0.6 / objc2-app-kit 0.3.

## 0. Prompt preamble (paste to the agent)

```
Rules for this build:
1. The daemon owns ALL state and speaks a typed JSON protocol over a local socket. The GUI is one more client. No business logic in the GUI (Rust shell or JS).
2. You cannot see the screen. Platform window/tray/permission behaviour cannot be reasoned out: add eprintln! diagnostics for every platform call and ask me to run the binary from a terminal; read the log before theorising.
3. Never call set_focus() on macOS; never set_focusable on a class-swapped window.
4. Bump the MINOR version whenever any protocol action is added/changed/removed (the GUI judges compatibility from the daemon's reported version).
5. Sign every macOS build (daemon, CLI, app) with the ONE stable certificate. Never rotate it.
6. Commit messages: plain text, no AI co-author trailers. Cut releases directly; use -rc tags for anything platform code that has not run on a real machine.
7. Do not check "it compiles on macOS" as "it works". Say which parts are unverified.
```

## 1. Purpose / when to use

Use when a background service must keep running with the UI closed (hotkeys, watchers, sync), and the UI is: menu-bar/tray dropdown + always-on-top floating button that expands into a panel + a normal main window (+ optional debug overlay). Tauri fits when the UI is HTML/JS and you accept the OS webview (WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux; NOT Chromium, so test CSS/JS features against the oldest of those, and do not assume Chrome-only APIs). Not suited if you need pixel-identical rendering across OSes.

## 2. Architecture

```
 +-------------------+      JSON over local socket       +--------------------+
 | daemon (copycatd) | <-------------------------------> | copycat-protocol   |
 | owns ALL state    |   Request{version,id,action,args} | (typed Action enum,|
 | log file, hotkeys |   -> Response/ResultBody          |  call/request/     |
 +-------------------+                                   |  is_running,       |
        ^   ^                                            |  VERSION)          |
        |   | spawn / version-check / takeover           +----------+---------+
        |   +----------------+                                      |
        |                    |                                      |
   CLI / TUI          +------+---------------------- Tauri shell (src-tauri/src/lib.rs) ---+
 (also clients)       | ensure_daemon()  poll thread: Status every 600ms -> emit("daemon-state")
                      | #[command] daemon(action,args) -> socket     tray, windows, panels
                      +--+----------+-----------+-----------+--------------------------+
                         |          |           |           |   (window.__TAURI__ in each)
                      panel(tray) main      floating[-N]  debug
                      index.html main.html  floating.html debug.html
```

Reusable invariants:
- One shared protocol crate (`crates/copycat-protocol`), `pub const VERSION: &str = env!("CARGO_PKG_VERSION")` (lib.rs:24), `PROTOCOL_VERSION: u32 = 1` (message.rs:10). GUI crate depends on it by path (`apps/copycat-gui/src-tauri/Cargo.toml`), so GUI and CLI cannot disagree on socket path or types.
- GUI is its own cargo workspace (`[workspace]` in its Cargo.toml; root `exclude = ["apps/copycat-gui"]`) so webkit2gtk never enters root CI on Linux.
- In Copycat this launcher code lives in `lib.rs` on main; on branch `gpui` it was extracted to `crates/copycat-launcher`. For a new app, put it in a launcher crate from day one so Tauri and GPUI front ends share it.

## 3. Build order (checklist)

1. [ ] Protocol crate: `Action` enum, `Request`, `Response`, `ResultBody`, `VERSION`, `default_socket_path()`, blocking `call()/request()/is_running()`.
2. [ ] Daemon: socket server, `Status` action returning `{daemon_version, ...your state}`, `DaemonStop` action, log file, `--version` printing `<name> <semver>`.
3. [ ] CLI proves every action works headless (UI adds nothing the CLI cannot do).
4. [ ] Tauri scaffold: `src-tauri/{Cargo.toml,build.rs,tauri.conf.json,capabilities/default.json,src/{main.rs,lib.rs}}`, `ui/`. `frontendDist` = `../ui` (no bundler; plain html/js/css).
5. [ ] `daemon` command + `ensure_daemon()` + poll thread + `daemon-state` event. Panel renders connected/offline.
6. [ ] Tray icon (template icon, left toggle, right menu) + panel window + `position_under_tray` + hide on blur.
7. [ ] Main window (hide-on-close, Dock toggle).
8. [ ] Floating button + expand + drag + per-screen copies.
9. [ ] macOS panel class swap for fullscreen Spaces (do last; needs a real Mac and diagnostics).
10. [ ] Sidecar bundling + CI `gui` job (stand-in sidecar) + `gui-release` workflow + signing action.
11. [ ] Doctor/health check surfacing OS permissions; Settings buttons that open the right pane and restart the daemon.
12. [ ] Ship `-rc` tag, have a human run it from a terminal, read log, iterate.

## 4. File anatomy

### 4.1 `Cargo.toml` (apps/copycat-gui/src-tauri)
```toml
[lib]
name = "copycat_gui_lib"
crate-type = ["staticlib", "cdylib", "rlib"]
[build-dependencies]
tauri-build = { version = "2", features = [] }
[dependencies]
tauri = { version = "2", features = ["tray-icon", "macos-private-api", "image-png"] }
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { version = "0.6", features = ["exception"] }
objc2-app-kit = { version = "0.3", default-features = false, features = ["std", "NSApplication", "NSPanel", "NSResponder", "NSWindow"] }
```
Use the objc2 versions tao already resolves (nothing new enters the build). `macos-private-api` is required for `transparent` windows on macOS and must match `"macOSPrivateApi": true` in tauri.conf.json. `main.rs`: `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` then `copycat_gui_lib::run()`. `build.rs`: `tauri_build::build()`.

### 4.2 `tauri.conf.json` windows (reusable shape)
- `"app.withGlobalTauri": true` so every page has `window.__TAURI__.core.invoke` / `.event.listen` with no bundler.
- `"build.frontendDist": "../ui"`; CSP: `default-src 'self'; style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' data:`.
- Four windows, all `"visible": false` (shown from Rust):

| label | url | size | notes |
|---|---|---|---|
| `panel` | index.html | 392x588 | tray dropdown: `decorations:false, transparent:true, shadow:false, alwaysOnTop, skipTaskbar, focus:true, resizable:false` |
| `main` | main.html | 940x640 (min 720x480) | normal decorated window, `center:true, dragDropEnabled:false` |
| `debug` | debug.html | 300x208 | overlay, same flags as panel but `focus:false` |
| `floating` | floating.html | 64x64 | `focus:false, focusable:false, acceptFirstMouse:true, dragDropEnabled:false`, transparent, alwaysOnTop |

`acceptFirstMouse:true` on the floating window: the first click must act, not just focus. The 64x64 includes a transparent margin the CSS shadow draws into.

### 4.3 `capabilities/default.json`
```json
{ "identifier": "default",
  "windows": ["panel", "main", "debug", "floating", "floating-*"],
  "permissions": ["core:default", "core:event:default"] }
```
The glob `floating-*` is mandatory: per-screen copies are built at runtime and a window not matched by any capability cannot `invoke` or `listen`. Custom `#[tauri::command]`s need no per-command permission here (app commands are allowed by default).

### 4.4 The single `daemon` command (lib.rs `daemon`)
```rust
#[tauri::command]
async fn daemon(app: tauri::AppHandle, action: String, args: Option<serde_json::Value>)
    -> Result<serde_json::Value, String> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("version".into(), serde_json::json!(1));
    envelope.insert("id".into(), serde_json::json!("gui"));
    envelope.insert("action".into(), serde_json::Value::String(action));
    if let Some(args) = args { envelope.insert("args".into(), args); }
    let req: Request = serde_json::from_value(serde_json::Value::Object(envelope))
        .map_err(|e| format!("bad request: {e}"))?;
    if injects_paste(&req.action) { hand_focus_back(&app); }
    request(&socket_path(), &req)
        .map(|body| serde_json::to_value(body).unwrap_or(serde_json::Value::Null))
        .map_err(|error| error.message)
}
```
- Envelope: `{version:1, id, action:"history.list", args:{...}}`; reply is the serialized `ResultBody`, tagged by `type` (`"clips"`, `"pasted"`, `"session_started"`...). Errors reject the invoke promise with a human message; JS shows it as a toast.
- `async` so the blocking socket call runs off the main thread. Send `null`, `{}` or nothing for no-arg actions: the protocol's tolerant deserializer accepts all.
- Frontend wrapper (ui/app.js): `invoke("daemon", { action, args: args ?? null })` inside try/catch -> `toast(String(message))`.
- Every window's JS goes through this one command; other commands (`open_main`, `open_settings`, `restart_daemon`, `set_debug_overlay`, `set_floating`, `floating_expand`, `move_window_by`) are window/OS chores only, all registered in `tauri::generate_handler![...]`.
- Copycat-specific: `injects_paste` list of Actions; replace with "actions that make the daemon type into the foreground app".

### 4.5 Daemon launcher (lib.rs `ensure_daemon`, `find_copycatd`, `semver_cmp`)
Rules (each cost a release):
1. If a daemon answers: `compatible = running_daemon_version(Status.daemon_version) >= protocol::VERSION` (semver precedence). Unknown version = incompatible.
2. Incompatible AND we have a replacement (`find_bundled().is_some() || compatible_installed().is_some()`): send `Action::DaemonStop`, wait up to 3s (30x100ms), start ours. No replacement: leave the user's only daemon alone.
3. Never stop a compatible daemon on GUI quit (shared service; GUI is a client, not owner).
4. Start order `compatible_installed() -> find_bundled() -> find_system_copycatd() -> "copycatd"`: installed first because the keychain ACL is bound to the creating binary; any other binary gets a prompt, and a refusal leaves it unable to read data. Search explicit dirs (`~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`): a Finder-launched GUI has a minimal PATH.
5. Spawn detached with stdio null (`Command::new(b).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()`); daemon has its own log and socket-in-use guard so racing starts are harmless. Wait up to 4s (40x100ms) for the socket.
6. Bundled = `current_exe().parent()/copycatd` with `len() > 0` (an empty file is the CI stand-in).
7. `semver_cmp`: pre-release < release (`0.5.0-rc.5 < 0.5.0 < 0.6.0-rc.1`); numeric ids compare numerically (`rc.10 > rc.9`), numeric < alphanumeric, `+build` ignored. A naive compare once let stale `0.5.0-rc.5` pass as `0.5.0`. Copy the function and the two `version_at_least` unit tests (`a_daemon_is_compatible_when...`, `a_pre_release_ranks_below...`) verbatim; they exercise `semver_cmp`.
8. Bump the MINOR when any action changes: compatibility is decided only from `daemon_version`; a same-minor daemon with a different action set looks compatible.

### 4.6 Poll thread (in `setup`)
```rust
let handle = app.handle().clone();
std::thread::spawn(move || {
    ensure_daemon();                       // before first poll: no "offline" flash
    loop {
        let installed = find_system_copycatd().is_some();
        let payload = match call(&socket_path(), Action::Status) {
            Ok(body) => serde_json::json!({ "connected": true, "installed": installed, "status": body }),
            Err(error) => serde_json::json!({ "connected": false, "installed": installed, "error": error.message }),
        };
        let _ = handle.emit("daemon-state", payload);   // needs `use tauri::Emitter`
        std::thread::sleep(Duration::from_millis(600));
    }
});
```
JS: `listen("daemon-state", (e) => render(e.payload))`. Polling (600ms) is deliberate v1; a protocol event stream is a later optimization. Windows that also fetch lists poll separately (`setInterval(refreshHistory, 1500)` in app.js). Every window (panel, main, floating, debug) listens independently; hidden windows still receive events.

### 4.7 Tray (setup)
```rust
#[cfg(target_os = "macos")]
let (icon, template) = (tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?, true);
#[cfg(not(target_os = "macos"))]
let (icon, template) = (app.default_window_icon().cloned().ok_or("no default window icon")?, false);

TrayIconBuilder::with_id("copycat")
    .icon(icon).icon_as_template(template)
    .menu(&menu).show_menu_on_left_click(false).tooltip("Copycat")
    .on_menu_event(|app, e| match e.id.as_ref() { "open" => open_main(app.clone()), "quit" => app.exit(0), _ => {} })
    .on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
            if let Some(w) = tray.app_handle().get_webview_window("panel") {
                if w.is_visible().unwrap_or(false) { let _ = w.hide(); }
                else {
                    position_under_tray(&w, rect);
                    show_over(&w);
                    #[cfg(not(target_os = "macos"))] let _ = w.set_focus();   // never on macOS
                }
            }
        }
    })
    .build(app)?;
```
- Left-click (on `Up`) toggles panel; right-click shows native menu (Open / Quit): the escape hatches that must always work. `show_menu_on_left_click(false)` is what separates the two.
- Template icon: `icons/tray-template.png` is 36x36 black + alpha only; macOS tints it for light/dark menu bar. Other OSes get the coloured app icon. `image-png` feature is needed for `Image::from_bytes`.
- Close on blur: `panel.on_window_event(move |e| if let WindowEvent::Focused(false) = e { let _ = hide_target.hide(); })`.
- Main window: intercept `WindowEvent::CloseRequested { api, .. }` -> `api.prevent_close(); hide();` and on macOS set `ActivationPolicy::Accessory` again. Quit only via tray menu (`app.exit(0)`). In `.run(|_app, event| ...)` only treat `ExitRequested { code: Some(_), .. }` / `Exit` as real exit (`code: None` = last window closed; the app keeps running).

### 4.8 Positioning the dropdown (`position_under_tray`, `place_under_icon`, `unit`) - REUSE VERBATIM
Facts established on a real Mac with a 2x laptop + 1x external:
- On macOS every rect Tauri reports (monitor position/size/work_area, the tray `rect`) is points x THAT screen's own scale. Two screens = two grids. Mixing them drops the panel on the wrong screen.
- `unit(scale) = if cfg!(macos) { scale } else { 1.0 }`; dividing a value by `unit(its own screen's scale)` returns AppKit's global point grid. Windows/Linux already use one pixel grid.
- The cursor position is reported with ONE fixed scale (primary's), so `pointer = cursor_position() / unit(primary_scale)` picks the clicked screen unambiguously. Fallback: guess from the icon rect.
- Result placed with `LogicalPosition` on macOS, `PhysicalPosition` elsewhere. x = icon centre - w/2 clamped to work area with 8pt margin; y = icon bottom + 2pt, clamped so a bottom Windows taskbar puts it above.
- Test (kept in lib.rs, macOS-only `#[cfg(target_os="macos")]`): laptop 3024x1964 @2x + external 1920x1080 @1x at x=1512; icon `(3000,0,30,24)`, pointer `(3010,10)`, window `(392,588)` -> `(2819,26)`; icon `(2800,0,60,48)`, pointer `(1410,10)` -> `(1112,26)`. Write such a numeric test for your own layout.

### 4.9 Floating button (set_floating, floating_expand, move_window_by, ui/floating.js)
- Constants: `FLOATING_BUTTON=(64,64)`, `FLOATING_PANEL=(340,500)` logical px. One window, two sizes. `body.className = "collapsed" | "expanded"`; grow the window BEFORE drawing the panel, hide the panel BEFORE shrinking, so neither state is drawn at the other's size.
- `floating_expand(window, expanded)`: keeps the bottom-right corner fixed, converts via `scale_factor()`, then clamps into `current_monitor().work_area()`.
- Click vs drag: JS `dragToMove(el,{onClick})` - pointer capture, 4px `Math.hypot` threshold on `screenX/Y`, then `invoke("move_window_by", {dx,dy})` (Rust adds `dx*scale` to `outer_position`). OS-level drag cannot start mid-gesture, hence the manual threshold. A click that ends a drag is swallowed in the `click` handler (keeps Enter/Space keyboard activation).
- Per-screen copies: `set_floating(on, every_screen)` is `#[tauri::command(async)]`; builds `floating-1..N` with `WebviewWindowBuilder` (same flags as the config entry, via `.focused(false).focusable(false).accept_first_mouse(true).disable_drag_drop_handler()`), one per non-primary monitor, OFF the main thread (building on it can deadlock on Windows); then `run_on_main_thread` to class-swap, park (`park_bottom_right`, 24pt inset on the placement grid), and show. Hide, never close, extras (tearing down a swapped panel throws). Known ceiling: monitor hot-plug needs a re-toggle (`ponytail:` comment in code).
- First placement guarded by a `static PLACED: AtomicBool`; afterwards the window stays where dragged. Persist the on/off toggles in `localStorage` (main.js `FLOATING_KEY`, `FLOATING_EVERY_KEY`) and call `applyFloating()` at main-window load: the main window (hidden) is what brings the button up at launch.

### 4.10 Paste focus hand-back (`hand_focus_back`)
Clicking any of your windows activates your app, so a keystroke the daemon injects lands in YOUR app. Before any action that makes the daemon type/paste into the foreground app:
```rust
let _ = app.run_on_main_thread(|| {
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        objc2_app_kit::NSApplication::sharedApplication(mtm).deactivate();
    }
});
std::thread::sleep(Duration::from_millis(150));   // settle; ponytail: use NSWorkspace activation notification if flaky
```
This is why `daemon` is `async` (the sleep must not freeze windows). macOS only; no-op elsewhere.

### 4.11 Activation policy / Dock icon
`setup`: `app.set_activation_policy(ActivationPolicy::Accessory)` (no Dock icon; activation does not drag the user off a fullscreen Space). `open_main`: set `Regular` (Dock icon + Cmd-Tab) then show/unminimize/set_focus (set_focus is fine here: the main window is a normal window). Main-window close: back to `Accessory`.

### 4.12 `open_settings` / `restart_daemon`
`open_settings(pane)` runs `open x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` (or `Privacy_ListenEvent`). `restart_daemon` = `DaemonStop`, wait up to 3s, `ensure_daemon()`. Needed because a CGEventTap is created once; a refusal sticks until a fresh process.

## 5. macOS specifics

### 5.1 Fullscreen overlays: NSPanel class swap (lib.rs `mod mac`, `float_everywhere`, `show_over`)
What was NOT enough (rc.15): `set_visible_on_all_workspaces` (CanJoinAllSpaces) + FullScreenAuxiliary + Accessory policy. What worked (confirmed on a real Mac, gui-v0.1.0-rc.17):
1. `define_class!` two NSPanel subclasses with NO ivars and no Drop (class gets only smaller than TaoWindow, so the swap is sound): `CopycatKeyPanel` (`canBecomeKeyWindow=true`, `canBecomeMainWindow=false`; for the dropdown, whose search box needs the keyboard) and `CopycatPassivePanel` (both false; floating button, debug overlay).
2. Accept BOTH `TaoWindow` and `NSKVONotifying_TaoWindow` (AppKit makes the KVO subclass once anything observes the window; on a real Mac always). rc.16 only accepted `TaoWindow` and silently did nothing. Check `AnyObject::class(ns_window).name()`; refuse and eprintln otherwise.
3. `objc2::ffi::object_setClass(object, panel)` (not `AnyObject::set_class`: it debug-asserts equal size; TaoWindow is one ivar bigger). Record `(object, original_class)` in a `static SWAPPED: Mutex<Vec<(usize,usize)>>`.
4. Then, inside `objc2::exception::catch` (an ObjC exception would abort the process): `setStyleMask(styleMask | NSWindowStyleMask::NonactivatingPanel)` and, if `respondsToSelector(sel!(_setPreventsActivation:))`, `msg_send![ns_window, _setPreventsActivation: true]` (private; the swap skipped NSPanel's init and setStyleMask alone does not tell the window server).
5. `setHidesOnDeactivate(false)`; `setCollectionBehavior(old | FullScreenAuxiliary | CanJoinAllApplications)`; LAST (NSPanel setters reset it) `setLevel(NSStatusWindowLevel)` (25 > fullscreen menu bar 24; tao alwaysOnTop is 5). Code carries a `ponytail:` note: Apple DTS says 1000 (NSScreenSaverWindowLevel) if 25 proves insufficient.
6. `show_over`: `window.show()` then `ns_window.orderFrontRegardless()`, and `makeKeyWindow()` only if `canBecomeKeyWindow()`. Never `set_focus` (activates the app, pulls the user off the fullscreen Space).
7. Never call tao's `set_focusable` afterwards (its ivar is gone; tao panics).
8. On exit (`RunEvent::ExitRequested{code:Some(_)}` or `Exit`), `mac::restore_classes()` puts the original classes back, else teardown throws an ObjC exception (crash on quit). Main thread only.
9. Call `float_everywhere` in `setup` (main thread, windows alive): panel with `takes_keys=true`; `debug` and `floating` with false; runtime-built `floating-N` inside `run_on_main_thread`.
10. Log class, level, collection behavior, style mask with `eprintln!` (present in code); this is how the KVO class was found.

### 5.2 Permissions (TCC)
- Attributed to the RESPONSIBLE app: Copycat.app when the app spawns the daemon, the terminal when the CLI does. The GUI's Settings button must tell the user to grant the app.
- Active CGEventTap + CGEventPost need Accessibility (not Input Monitoring; do not send users to the wrong pane). `CGEventTapCreate` returns non-null even when refused, so the daemon must ask the OS (`AXIsProcessTrusted`, `CGPreflight*`; see `crates/copycat-daemon/src/platform/mac_permissions.rs`) and expose it in a doctor/health check the GUI shows.
- After a grant, `restart_daemon`.
- Event-tap run loop: run in default mode (`CFRunLoop::run_current()`) while `add_source` to the common set; running in `kCFRunLoopCommonModes` spins and delivers nothing (log line: `invalid mode 'kCFRunLoopCommonModes' provided to CFRunLoopRunSpecific`).
- Signing: ad-hoc/unsigned builds change identity each build, so grants go stale. Sign daemon, CLI and app with ONE stable self-signed certificate; see section 8.
- `MACOSX_DEPLOYMENT_TARGET=10.15` in workflow `env` (preflight symbols); `bundle.macOS.minimumSystemVersion: "10.15"`.

### 5.3 Icons
`bundle.icon`: `32x32.png`, `128x128.png`, `128x128@2x.png` (256x256), `icon.icns`, `icon.ico` (6 sizes from 16). `icon.png` 512 is the source (`cargo tauri icon` can regenerate). Tray template: separate 36x36 black+alpha PNG (source SVG `copycat-package/brand/selected/copycat-menubar-template.svg`). Windows/Linux tray uses `default_window_icon()`.

## 6. Sidecar bundling
- `bundle.externalBin: ["binaries/copycatd"]`. Tauri requires a file named `binaries/copycatd-<target-triple>` at build time (e.g. `copycatd-aarch64-apple-darwin`); in the bundle it is installed as `Contents/MacOS/copycatd` (what `find_bundled()` expects beside `current_exe()`).
- Universal macOS build (`--target universal-apple-darwin`) needs THREE files: `copycatd-aarch64-apple-darwin`, `copycatd-x86_64-apple-darwin` (per-arch compiles) and a `lipo -create` result named `copycatd-universal-apple-darwin` (final copy). Missing any = build error.
- `binaries/` and `gen/` are git-ignored. CI that only compiles the app uses an empty stand-in: `touch binaries/copycatd-aarch64-apple-darwin && chmod +x ...` (tauri-build refuses to run otherwise); `find_bundled` treats empty files as absent.
- The daemon is spawned detached, but macOS still attributes its TCC permissions to the app that spawned it.

## 7. Verification without seeing the screen
1. Pure logic (`semver_cmp`, `place_under_icon`) in `#[cfg(test)]` with numeric cases (lib.rs tests). Run on Linux where compilable; the GUI crate needs webkit2gtk to build on Linux, so otherwise rely on macOS CI job `gui`.
2. UI logic with Playwright (no Tauri runtime): serve `apps/copycat-gui/ui` with a local static server (e.g. `python3 -m http.server`), and before page scripts run inject a mock via `page.addInitScript`: `window.__TAURI__ = { core: { invoke: async (cmd, args) => /* canned ResultBody by action */ }, event: { listen: (name, cb) => { window.__emit = (n,p)=>cb({payload:p}); return Promise.resolve(()=>{}); } } }`. Then call `__emit("daemon-state", {connected:true, status:{...}})` and assert DOM/screenshot; log `invoke` calls to assert the wire args (`daemon` with `{action,args}`). This covers rendering, offline state, click-vs-drag threshold, expand sequence. It cannot cover window placement, panels, tray, permissions. (A working mock and screenshot script are committed: `docs/guides/tools/mock-tauri.js` and `ref-shots.js`. A page destructures at load; mock what each page destructures at load: `const { invoke } = window.__TAURI__.core; const { listen } = window.__TAURI__.event;`.)
3. macOS type-check from Linux ("cross-clippy"): `docs/guides/tools/mac-check.sh <Cargo.toml>` (stub `cc`/`ar` in `tools/fake-cc.sh` and `tools/fake-ar.sh`, `CARGO_FEATURE_PURE=1`, target `aarch64-apple-darwin`, `-D warnings`). Create the placeholder sidecar first (`touch src-tauri/binaries/<name>-aarch64-apple-darwin`). It checks the `#[cfg(target_os="macos")]` code (objc2 types, `define_class!`) without linking; it proves types only.
4. Real behaviour: CI on `macos-14` compiles (`ci.yml` job `gui`); then a `-rc` tag; the human runs `/Applications/<App>.app/Contents/MacOS/<bin>` from a terminal so `eprintln!` output is visible, and pastes the daemon log. Instrument first, then read; do not theorise across releases (the run-loop-mode bug and the KVO-class bug were both found in one log line).
5. Daemon side: `copycat logs` / `copycatd --log debug`; absence of a log line for a key press = the OS is not delivering it; presence = bug is downstream.

## 8. CI / release / signing
- `ci.yml`: `test` (ubuntu: `cargo test --workspace --locked`, clippy `-D warnings`), `check` matrix (linux-arm, macos-14 x2 targets, windows) clippy per target, `gui` job on `macos-14`: stand-in sidecar then `cargo build --manifest-path apps/copycat-gui/src-tauri/Cargo.toml`.
- Tag namespaces so pipelines never cross-fire: `v*` -> `release.yml` (daemon + CLI archives, `-` in tag => `--prerelease`), `gui-v*` -> `gui-release.yml` (app; `workflow_dispatch` input `tag`; checkout `ref: ${{ github.event.inputs.tag || github.ref }}`).
- `gui-release.yml` (runs-on `macos-14`, env `MACOSX_DEPLOYMENT_TARGET: "10.15"`): toolchain with both apple targets; `Swatinem/rust-cache` for both workspaces (`.` and `apps/copycat-gui/src-tauri`); build daemon for both triples, copy to `binaries/`, `lipo`; `uses: ./.github/actions/macos-signing` with secrets; `tauri-apps/tauri-action@v0` with `projectPath: apps/copycat-gui/src-tauri`, `args: --target universal-apple-darwin`, `prerelease: true`, env `APPLE_SIGNING_IDENTITY: ${{ steps.sign.outputs.identity }}` (overrides the config's `"signingIdentity": "-"`); `bundle.targets: ["app","dmg"]`; `hardenedRuntime:false`. Final step `codesign -dvv` the .app and `Contents/MacOS/copycatd` and, if identity != "-", `grep -q "Authority=$IDENTITY"` so a silently ad-hoc build turns the run red.
- `.github/actions/macos-signing/action.yml` (composite): inputs `certificate` (base64 p12), `password`; creates a temp keychain (`security create-keychain`, `set-keychain-settings -lut 21600`, `import -T /usr/bin/codesign`, `set-key-partition-list -S apple-tool:,apple:,codesign:`), adds it to the user search list, trusts the cert runner-only (`sudo security add-trusted-cert -d -r trustRoot -p codeSign`); outputs `identity` = cert common name ("Copycat Code Signing"), or `-` with a warning if no secret (forks). Repo secrets `MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`. Same action is used in `release.yml` (it runs `codesign --force --sign "$IDENTITY"` on daemon/CLI).
- Create the cert ONCE (self-signed, code-signing EKU, long validity), keep the .p12 + password backed up outside the repo, never rotate casually: a new cert means every user re-grants (`tccutil reset Accessibility <bundle-id>`). Not notarized: release notes tell users `xattr -dr com.apple.quarantine /Applications/<App>.app` or right-click -> Open.
- Release discipline: cut tags directly (no asking); any release containing platform code not run on a real machine is `-rc.N` (`gui-v0.1.0-rc.17`, `v0.6.0-rc.3`) and `prerelease: true`. `install.sh` resolves `/releases/latest` and so skips pre-releases: pin with `COPYCAT_VERSION=v0.6.0-rc.3`. Bump daemon minor when protocol changes. Bump `tauri.conf.json` `version` with the GUI tag.
- Waiting on CI: `gh run watch <id> --exit-status` WITHOUT piping into `tail`/`head` (the pipe masks the exit status); check `gh run view <id> --json conclusion`.
- Commits: plain messages, no co-author/AI trailers.

## 9. Pitfalls we hit

| Symptom | Cause | Fix |
|---|---|---|
| Panel/overlay vanishes or user jumps off a fullscreen app's Space | Activating the app / ordinary NSWindow cannot join another app's fullscreen Space | NSPanel class swap + NonactivatingPanel + status level; Accessory policy; no `set_focus` on macOS (5.1) |
| Class swap did nothing (rc.16) | Guard accepted only `TaoWindow`; real Macs have `NSKVONotifying_TaoWindow` | Accept both; eprintln the class |
| Crash on quit after swap | Tearing down a window whose KVO subclass was swapped throws | `restore_classes()` on exit |
| Tao panic after swap | Called `set_focusable` on a swapped window (ivar gone) | Never call it |
| Paste lands in your own app | Clicking your window activated it | `NSApplication.deactivate()` + 150ms before paste actions |
| Dropdown opens on wrong screen / far from icon | Rects are points x each screen's own scale on macOS | `unit()` normalisation, pointer picks screen (4.8) |
| Floating window cannot invoke/listen | Runtime label not in capability `windows` | Glob `floating-*` |
| Deadlock building a window on Windows | Built on main thread | `#[tauri::command(async)]`, build off-thread, `run_on_main_thread` for AppKit |
| Stale old daemon accepted | Pre-release ignored in compare | `semver_cmp` precedence; tests |
| Keychain prompt / unreadable data | Daemon started from a different binary than the one that created the item | Prefer compatible installed daemon over bundled |
| GUI thinks daemon is fine but action missing | Action added without version bump | Bump minor on any action change |
| Accessibility grant lost every update | Ad-hoc signing changes identity | One stable cert via `macos-signing` action |
| Hotkeys silent though "doctor ok" | Event tap returns non-null when refused; wrong pane; run loop in common-modes | Ask OS (`AXIsProcessTrusted`), Accessibility pane, default-mode run, restart daemon after grant |
| Preflight symbols fail to load | Deployment target < 10.15 | `MACOSX_DEPLOYMENT_TARGET: "10.15"` |
| tauri-build fails in CI | `externalBin` file missing for the triple | Stand-in empty sidecar; universal build needs 3 names |
| Release run shows green after failure | `gh run watch | tail` masks status | Do not pipe; `--exit-status` |
| `install.sh` ignores your rc | Pre-releases are not "latest" | Pin `COPYCAT_VERSION` |
| Tray icon dark-on-dark | Coloured icon on macOS | Template PNG + `icon_as_template(true)` |
| Left click opens menu not panel | Default tray behaviour | `show_menu_on_left_click(false)` |
| Transparent window opaque on macOS | Missing private API | `macos-private-api` feature + `"macOSPrivateApi": true` |
| Close button quits the app | Default close | `CloseRequested` -> `prevent_close` + hide |
| Floating drag fires a click | No threshold | 4px threshold, swallow the post-drag click |
| Fonts/CSS differ across OS | Native webviews, not Chromium | Test on WKWebView/WebView2/WebKitGTK; avoid Chrome-only APIs |
| New screen has no button | Monitors enumerated only at toggle | Re-toggle (known limit) or wire monitor events |

## 10. Copycat-specific vs reusable

Reusable: sections 2, 4.1-4.3, 4.5-4.12, 5, 6, 8. Copycat-specific: action names (`history.list`, `paste.id`, `stack.start`, `queue.seal`...), `injects_paste` list, `FLOATING_*` sizes, 392x588 panel, UI content, icon art, bundle id `dev.copycat.desktop`, cert name "Copycat Code Signing", `copycatd` binary name.

## 11. Acceptance checklist

- [ ] GUI contains no business logic; every UI action is a daemon `Action`; CLI can do the same.
- [ ] `VERSION = CARGO_PKG_VERSION`; Status returns `daemon_version`; minor bumped for protocol changes; `semver_cmp` tests pass.
- [ ] Launch with no daemon: GUI starts one, no "offline" flash. Launch with older daemon + bundled sidecar: replaced. Quit GUI: compatible daemon keeps running.
- [ ] Installed compatible daemon preferred over bundled.
- [ ] `daemon-state` reaches every window within ~600ms; offline state renders.
- [ ] Tray: template icon tints in light/dark; left toggles panel under the icon on each display (mixed scales); right shows Open/Quit; blur closes panel.
- [ ] Main window: Dock icon while open; close hides; quit only from tray menu; no crash on quit.
- [ ] Floating button: click expands in place (corner fixed, clamped), drag moves without clicking, per-screen toggle works, hidden not closed.
- [ ] Over a fullscreen app: panel, floating button, overlay visible; no Space switch; log shows class `CopycatKeyPanel`/`CopycatPassivePanel`, level 25.
- [ ] Paste/type actions land in the previously active app.
- [ ] Doctor shows Accessibility state from the OS; Settings button opens the right pane; restart-daemon works.
- [ ] Capabilities include `floating-*`; CSP set; no remote script.
- [ ] Sidecar bundled (3 names for universal); CI has stand-in; gui job green on macos-14.
- [ ] Release artifacts signed with the stable cert (`codesign -dvv` shows Authority); grant survives an update.
- [ ] Tagged `-rc` until a human ran it on a real machine; tag namespaces separate; install.sh pinned for rc.
