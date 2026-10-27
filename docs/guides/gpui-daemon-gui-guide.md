# GUIDE: daemon + GPUI front end (tray dropdown, floating button, main window)

Audience: a coding agent building a NEW app shaped like Copycat. Worked example: `apps/copycat-gpui` and `crates/copycat-launcher`, both on `main`. Rules marked **[Copycat]** are specific to that app; everything else is the reusable pattern. Sibling guide: the Tauri one (same daemon, same release pipeline). Pick one front end per app; GPUI is native and has no webview, but is immature (see Pitfalls) and macOS-first.

## 0. Prompt preamble (paste to the agent)

```
You are building <APP>: an always-running daemon (<app>d), a GPUI GUI client, a floating button that
expands into a panel, and a menu-bar dropdown. Rules:
1. The daemon owns ALL state. UIs are clients of one typed JSON protocol over a local socket. No state in the GUI except prefs.
2. You cannot see the screen and you cannot test macOS here. Never theorize about platform UI behaviour:
   add eprintln diagnostics, ship, and ask the user to run the binary from a terminal and paste the output/daemon log.
3. Pin gpui-kit with "=x.y.z" and never list gpui directly. The GUI is its own cargo workspace, excluded from the root.
4. Never block the UI thread: every socket call goes through cx.background_spawn and returns via WeakEntity::update.
5. macOS: sign EVERY binary with ONE stable self-signed cert; MACOSX_DEPLOYMENT_TARGET=10.15; never rotate the cert.
6. Bump the MINOR version whenever a protocol Action/ResultBody changes (the GUI judges compatibility by daemon_version).
7. Plain commit messages, no AI co-author trailers. Cut releases directly; use -rc for anything not run on a real machine.
8. Build the UI from real gpui-component widgets (Sidebar, Button, Switch, Tag, Kbd, GroupBox, Icon, notifications) with the brand set in the theme. Plain divs + hex colours + glyph characters make it feel like a webapp: see section 3.4.
9. Follow the build order and acceptance checklist in this guide; report what you could not verify.
```

## 1. Architecture

```
            +-------------------- user's Mac -----------------------+
 CLI ------>|                                                        |
 TUI ------>|   <app>d  (daemon: clipboard, hotkeys, event tap,      |
 GPUI app ->|   state, keychain)  <-- local socket, JSON frames -->  |
            |        ^   crates/<app>-protocol: Action/ResultBody,   |
            |        |   VERSION = env!("CARGO_PKG_VERSION")         |
            |   crates/<app>-launcher: find / version-check / start  |
            +--------|-----------------------------------------------+
   GPUI process (apps/<app>-gpui, own cargo workspace)
     main thread: gpui App loop + tray-icon (AppKit) + NSWindow tweaks
       Entity<Daemon>  <- ONE poll task (Action::Status every 600ms, background_spawn)
         ^observe   ^observe        ^observe          ^observe
       AppView    Dropdown       Floating(slot n)    Overlay
       (main win) (PopUp panel)  (PopUp, 2 sizes)    (PopUp)
       Entity<Settings>  <- gui.json (prefs only)
```

Source: `app.rs::Daemon`, `main.rs`, `tray.rs`, `floating.rs`.

Protocol facts (`crates/copycat-protocol/src/lib.rs`): `pub const VERSION: &str = env!("CARGO_PKG_VERSION")`; re-exports `call`, `default_socket_path`, `is_running`, `Action`, `ResultBody`, `PROTOCOL_VERSION`. `call(&socket, Action) -> Result<ResultBody, CoreError>` is blocking.

## 2. Build order (checklist)

- [ ] 1. Workspace: `crates/<app>-protocol`, `-core`, `-daemon`, `-cli`, `-launcher`. Protocol `VERSION` from `CARGO_PKG_VERSION`; `[workspace.package] version` is the single source.
- [ ] 2. Daemon works headless with a file-backed stand-in for the OS resource (Copycat: `--clipboard file --clipboard-file <path>`, polled every 250ms). This is what makes CI screenshots possible; build it before any UI.
- [ ] 3. `<app>-launcher` (section 4.1). Both front ends share it.
- [ ] 4. GUI crate `apps/<app>-gpui`: own `[workspace]`, root `exclude = [...]`.
- [ ] 5. Bootstrap (`main.rs`), `Daemon` entity + poll, `request()` helper.
- [ ] 6. Brand/theme (`brand.rs`), main window screens.
- [ ] 7. Settings entity + `gui.json`.
- [ ] 8. macOS: tray + dropdown (`tray.rs`, `dropdown.rs`).
- [ ] 9. macOS: floating button/panel + overlay (`floating.rs`, `overlay.rs`), native placement.
- [ ] 10. Env-gated screenshot hooks, CI workflow `gpui.yml` (section 7).
- [ ] 11. Doctor/health check in the daemon that asks the OS for permissions (section 6) and a GUI callout.
- [ ] 12. Packaging, signing, bundled daemon (section 8: **not done in Copycat's GPUI app**).

## 3. File anatomy

```
apps/<app>-gpui/
  Cargo.toml  Cargo.lock  README.md
  assets/{fonts/*.ttf + OFL-*.txt, mark.svg, tray-template.png}
  src/main.rs      bootstrap, open_main, cfg gating
  src/app.rs       Daemon entity, AppView, request(), injects_paste
  src/brand.rs     tokens, theme, fonts, mark, shared widgets
  src/screens.rs   History/Session/Settings views, DragRow/DragGhost
  src/bindings.rs  Bindings screen + form          [Copycat]
  src/settings.rs  gui.json prefs
  src/tray.rs      menu-bar icon, dropdown placement (macOS)
  src/dropdown.rs  tray panel view (macOS)
  src/floating.rs  floating button/panel, native NSWindow helpers, sync (macOS)
  src/overlay.rs   debug overlay (macOS)           [Copycat]
```

### 3.1 `Cargo.toml` (real)

```toml
[package]
name = "copycat-gpui"
edition = "2024"

# Its own workspace, detached from the repo root (root Cargo.toml: exclude = ["apps/copycat-gui", "apps/copycat-gpui"]).
# GPUI's Linux build needs Vulkan, Wayland and xkbcommon headers; they must not enter the root build or CI.
[workspace]

[dependencies]
copycat-protocol = { path = "../../crates/copycat-protocol" }
copycat-core = { path = "../../crates/copycat-core" }
copycat-launcher = { path = "../../crates/copycat-launcher" }
# One dependency for the whole GPUI stack; the app never lists gpui itself.
gpui-kit = "=0.8.0"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[target.'cfg(target_os = "macos")'.dependencies]
tray-icon = { version = "0.25", default-features = false }   # default features are Linux-only (libxdo, appindicator)
async-channel = "2"
png = "0.18"
objc2 = "0.6"
objc2-foundation = { version = "0.3", default-features = false, features = ["std", "NSDictionary", "NSValue", "NSString", "NSGeometry"] }
objc2-app-kit = { version = "0.3", default-features = false, features = ["std", "NSApplication", "NSResponder", "NSStatusItem", "NSStatusBarButton", "NSButton", "NSControl", "NSView", "NSWindow", "NSScreen", "NSGraphics", "NSEvent", "NSPanel"] }
raw-window-handle = "0.6"
```

Pinning: `gpui-kit =0.8.0` wraps `gpui-pre 0.3.9` (a snapshot of zed, registry crates `gpui-pre-*`) plus `gpui-component 0.8.0` and `gpui-base 0.8.0`. Exact `=` because pre-1.0 GPUI breaks APIs between minors. Add objc2 features only as you use them (each class is feature-gated).

### 3.2 `main.rs` bootstrap (real, trimmed)

```rust
use gpui_kit::*;

fn main() {
    let app = gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)            // without it every icon renders blank
        .with_quit_mode(if cfg!(target_os = "macos") { QuitMode::Explicit }       // tray keeps the app alive
                        else { QuitMode::LastWindowClosed });                      // no tray elsewhere yet
    #[cfg(target_os = "macos")]
    app.on_reopen(tray::show_main);                       // Dock click with no window reopens main
    app.run(|cx| {
        gpui_kit::init(cx);            // 1. component + base init, before anything else
        brand::init(cx);               // 2. fonts + theme, before any window
        settings::Settings::start(cx); // 3. prefs global, before windows read it
        app::Daemon::start(cx);        // 4. single poll, before windows observe it
        let main = open_main(cx);
        #[cfg(target_os = "macos")]
        { tray::install(main, cx); floating::start(cx); }
        cx.activate(true);
    });
}

fn open_main(cx: &mut App) -> AnyWindowHandle {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some("Copycat".into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::centered(size(px(940.), px(640.)), cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| app::AppView::new(window, cx)))
        .expect("failed to open the window").0
}
```

`gpui_kit::open_window` wraps your view in `gpui_base::Root` (`gpui-kit-0.8.0/src/lib.rs`); the builder returns your content, **not** a Root. Always open windows through it, including popups. Call `init` before opening any window; inside async code call it within `cx.update`.

### 3.3 `app.rs`: shared `Daemon` entity, one poll, `request()`

```rust
pub struct Daemon { pub connected: bool, pub status: Option<StatusReport>, pub error: Option<SharedString>, _poll: Task<()> }
struct SharedDaemon(Entity<Daemon>);
impl Global for SharedDaemon {}

impl Daemon {
    pub fn start(cx: &mut App) {                              // once, before any window
        let daemon = cx.new(|cx| Daemon { connected: false, status: None, error: None,
            _poll: cx.spawn(async move |this: WeakEntity<Daemon>, cx| {
                cx.background_spawn(async { copycat_launcher::ensure_daemon() }).await;   // up before first poll
                loop {
                    let reply = cx.background_spawn(async { call(&copycat_launcher::socket_path(), Action::Status) }).await;
                    let polled = this.update(cx, |d, cx| {
                        (d.connected, d.status) = match reply { Ok(ResultBody::Status(s)) => (true, Some(*s)), _ => (false, None) };
                        cx.notify();
                    });
                    if polled.is_err() { break; }              // entity dropped
                    cx.background_executor().timer(Duration::from_millis(600)).await;
                }
            }) });
        cx.set_global(SharedDaemon(daemon));
    }
    pub fn get(cx: &App) -> Entity<Self> { cx.global::<SharedDaemon>().0.clone() }
}

pub fn request<V: 'static>(action: Action, cx: &mut Context<V>,
    done: impl FnOnce(&mut V, Result<ResultBody, CoreError>, &mut Context<V>) + 'static) {
    let socket = copycat_launcher::socket_path();
    cx.spawn(async move |this, cx| {
        let reply = cx.background_spawn(async move { call(&socket, action) }).await;   // blocking socket off the UI thread
        let _ = this.update(cx, |view, cx| { done(view, reply, cx); cx.notify(); });
    }).detach();
}
```

Rules distilled from the code:
- **Every view `cx.observe(&daemon, ...)`**; none polls. Dropdown/Floating/Overlay/AppView all read the one entity. Keep the `_poll: Task` as a field (dropping it cancels it).
- `AppView::new` calls `on_daemon(cx)` once so a window reopened from the tray starts from the last poll, not blank.
- Derive "did the list change" from status, not timers: `history_mark(status) = (latest clip id, hot_items)`; refetch only when it moves. Doctor: on connect, when a session starts, about every 10s (`polls >= 16`).
- Responses for a session that ended while the request was out are dropped (`items_session` tag). In-flight flag (`items_loading`) must be cleared on error too, so use `request` directly, not a helper that skips the callback on error.
- Debounce with a replaced `Task` (`_search = Some(cx.spawn(... timer(200ms) ...))`); toasts go through `window.push_notification` (the old hand-made toast used a generation counter so an old timer could not clear a new toast).
- Actions that make the daemon inject a keystroke (paste) must first hand focus back to the previous app: `NSApplication::sharedApplication(mtm).deactivate()` (`tray::deactivate`), then `request` sleeps 150ms on the background thread (`injects_paste` gates this). Not `cx.hide()` (a hidden app opens the dropdown invisibly). Skip when offline. A failed paste re-activates the window and toasts the error; from the dropdown (which closes first) the error is parked in `Daemon.error` and shown by the main window via `cx.defer(tray::show_main)`.

### 3.4 `brand.rs`: theme, fonts, logo

- Tokens as `pub const X: u32 = 0xRRGGBB` used with `rgb(X)`; alpha ones as `0xRRGGBBAA` with `rgba(X)`. **[Copycat]** values (GROUND `0x131519`, ACTIVE `0xE4672B`, ...) come from the Tauri `main.css`; copy the pattern, not the values.
- `init(cx)`: `cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!(..)), ..])` registers bundled TTFs (do not rely on system fonts; none of Copycat's three ship with macOS; license texts alongside). Then `Theme::change(ThemeMode::Dark, None, cx)` **first** (switching mode reloads the registered theme over edits), then `Theme::update(cx, |theme| { theme.font_family = ..; theme.colors.primary = rgb(ACTIVE).into(); ... })`. Forced dark: never follow the OS appearance unless you build both token sets.
- Logo: **use an `img`, not `svg()`** (`svg()` paints a one-colour mask):

```rust
pub fn mark(size: f32) -> Img {
    static MARK: LazyLock<Arc<Image>> = LazyLock::new(|| Arc::new(Image::from_bytes(ImageFormat::Svg, include_bytes!("../assets/mark.svg").to_vec())));
    img(MARK.clone()).size(px(size)).flex_none()
}
```
  The SVG declares the size it is drawn at, so it rasterizes crisp.
- **Use the real gpui-component widgets; do not rebuild the web design out of `div()`s.** A GPUI app drawn from divs, hex colours and text glyphs reads as a webapp in a native window. Put the brand in the *theme* (`Theme::update` in `brand::init`: background, border, primary, secondary, sidebar, popover, radius, fonts) and let stock widgets inherit it. Use: `Sidebar`/`SidebarMenu` for navigation, `TitleBar` for the window frame, `Button` (`.primary()/.ghost()/.outline()/.danger()/.small()`, `.icon(IconName::..)`, `.tooltip(..)`), `Switch` for on/off settings (not a checkbox), `Checkbox` for multi-select, `Tag`/`Badge` for status chips, `Kbd` for key caps, `GroupBox` for cards, `v_form()`/`field()` for forms, `Alert` for banners, `window.push_notification` for toasts, `Icon`/`IconName` instead of glyph characters, `Tooltip` on icon-only buttons. Colours come from `cx.theme()`, not inline `rgb(0x..)`.
- Custom drawing is only for what no widget covers: the round floating button, pointer-anchored popup windows, the drag ghost, the brand mark. Even those get their contents from widgets.
- Text inputs: `gpui_kit::component::input::{Input, InputState, InputEvent}`; scrolling: `use ...scroll::ScrollableElement as _;` then `.overflow_y_scrollbar()`.
- Shared builders (thin wrappers returning real widgets): `button(id, label, Kind, small) -> Button`, `primary/ghost`, `icon_button(id, IconName) -> Button`, `section(title) -> GroupBox`, `heading`, `dot(connected, ring)`, `mono(size)`. If you need a style the theme cannot give, change the theme first.
- Review test: delete the screenshots' colours in your head. If the layout still looks like a native settings/sidebar app, good; if it only looks right because of hand-placed paddings, you rebuilt CSS.

### 3.5 Component / GPUI gotchas (all from code or its git history)

| Rule | Evidence |
|---|---|
| **One `.hover()` per element.** A second panics in debug builds (CI is debug). Builders that already set hover must take the hover as a parameter. | commit 088c94c; `glyph_button_hover(.., hover: fn(StyleRefinement)->StyleRefinement)` |
| Stateful behaviour (`on_click`, `hover`, `on_drag`, `id`) needs `.id(..)`; list items need unique ids: `(("pin", ix))`. | `button(("pin", ix), ..)` |
| Glob imports clash: files use `use gpui_kit::prelude::*; use gpui_kit::*;` plus trait imports as `as _` (`ScrollableElement as _`, `Disableable as _`). Add traits anonymously; if a name is ambiguous, import the specific path (`gpui_kit::component::{h_flex, v_flex}`) instead of widening a glob. | `app.rs` / `screens.rs` heads |
| No `letter-spacing`: render one element per char: `h_flex().gap(px(s)).children(text.chars().map(\|c\| c.to_string()))`. | `brand::spaced` |
| A border has one colour: a coloured left edge next to another border colour = two nested boxes. | `brand::accent_edge` |
| Fonts may lack glyphs: JetBrains Mono has no U+2606, so both stars fall back to the filled one; carry state in colour. Check every symbol (▲ ▼ ★ ⠿ – ◀ ▶) renders in the screenshot. | `screens.rs` comment near the pin button |
| `clamp(min,max)` panics when min > max (a status poll mid-drag moved the cursor past the last item). Use `.min(last).max(cursor)`. | commit 9171ba1 |
| Opaque windows have no rounded corners/shadow; make the popup `WindowBackgroundAppearance::Transparent` and draw your own shadow with `BoxShadow` inside an 8px margin (that is why `BUTTON` is 64 for a 48 bubble). | `floating.rs` |
| `focus: false` on non-activating popups, else they steal focus from the user's app. | `floating::popup` |

### 3.6 Drag and drop (reorder)

```rust
#[derive(Clone)] pub struct DragRow { pub ix: usize, pub id: ClipId }   // id lets the daemon refuse a stale move
pub struct DragGhost(pub SharedString);                                  // a Render view that follows the pointer
row.on_drag(DragRow { ix, id }, move |_, _, _, cx| { let t = ghost.clone(); cx.new(|_| DragGhost(t)) })
   .drag_over::<DragRow>(move |style, from, _, _| if from.ix == ix { style } else { style.shadow(marker(..)) })  // drop marker
   .on_drop(cx.listener(move |this, from: &DragRow, _, cx| this.move_item(from.ix, from.id, ix, cx)))
```
Always also provide up/down buttons (accessible, testable without a pointer). Disable dragging for already-consumed rows.

### 3.7 `settings.rs`: prefs (not daemon config)

`#[derive(Serialize, Deserialize)] #[serde(default)] struct Settings { page_size, floating, every_screen, debug }`; shared as `Entity<Settings>` in a Global; `change(cx, |s| ..)` edits, clamps, writes `~/Library/Application Support/<app>/gui.json` (macOS) or `$XDG_CONFIG_HOME|~/.config/<app>/gui.json`, then `cx.notify()`. Parse failure = defaults; clamp values that are divisors. `floating::start` does `cx.observe(&Settings::get(cx), |_, cx| sync(cx))` so toggles open/close windows live. There is a unit test for round-trip, bad file, missing keys (`settings.rs` tests): keep one.

### 3.8 `tray.rs` (macOS): menu-bar icon and dropdown

Constraints: tray-icon handlers run on the AppKit main thread, must be `Send + Sync`, get no `&mut App`. So they only post to a channel; a GPUI task drains it.

```rust
let icon = TrayIconBuilder::new().with_icon(template_icon()).with_icon_as_template(true)   // black+alpha PNG, OS tints it
    .with_menu(Box::new(menu)).with_menu_on_left_click(false)                               // left = dropdown, right = menu
    .with_tooltip("Copycat").build().expect("tray icon");
let (tx, rx) = async_channel::unbounded();
let icon_tx = tx.clone();                                               // (the menu handler below moves `tx`)
TrayIconEvent::set_event_handler(Some(move |e| {
    // click on RELEASE only: one physical click delivers both Down and Up
    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e { let _ = icon_tx.try_send(TrayMsg::Toggle); }
}));
cx.set_global(Tray { icon, main, panel: None, blurred_at: None });      // GLOBAL KEEPALIVE: dropping TrayIcon removes the icon
cx.spawn(async move |cx| { while let Ok(msg) = rx.recv().await { cx.update(|cx| match msg { /* toggle / show_main / cx.quit() */ }); } }).detach();
```
- Menu items must always work: "Open" and "Quit" (`cx.quit()` posts `terminate:`, so it does not re-enter).
- **Debounce**: clicking the icon blurs the panel (closing it) before the release arrives; `blurred_at` + 300ms stops the same click reopening it. Panel handle `take()` first; if `remove_window()` succeeds the click was a close.
- Dropdown window: `WindowKind::PopUp`, `titlebar: None`, `focus: true` (it must take key to get blur), `is_movable/resizable/minimizable: false`. Close on blur via `cx.observe_window_activation(window, ..)` but **only after it has been active once** (`was_active`): GPUI can resign key status while opening.
- Icon: `include_bytes!("../assets/tray-template.png")` decoded with `png` to RGBA, then `Icon::from_rgba`.

### 3.9 Placing the dropdown (coordinates)

Do not use tray-icon's click rect (points x backing scale, flipped against the main display: wrong on multi-display/Retina). Read the status item instead:

```rust
let window = icon.ns_status_item()?.button(mtm)?.window()?;
let icon_frame = window.frame();            // Cocoa global points, y UP
let screen = window.screen()?; let frame = screen.frame();
let number = screen.deviceDescription().objectForKey(&NSString::from_str("NSScreenNumber"))?;
let id = number.downcast_ref::<NSNumber>()?.unsignedIntValue();                 // == GPUI DisplayId
let x = (icon_frame.origin.x + icon_frame.size.width/2.0 - width/2.0 - frame.origin.x).clamp(0.0, (frame.size.width - width).max(0.0));
let y = frame.origin.y + frame.size.height - icon_frame.origin.y;               // measured DOWN from the screen top
open(Some(DisplayId::new(id as u64)), point(px(x as f32), px(y as f32)), cx)
```
GPUI window origin is points from the top-left **of the display named by `display_id`**; Cocoa is bottom-left y-up in a global space. Convert once, here. Avoid newer-only API (`CGDirectDisplayID()` on NSScreen).

### 3.10 Popup windows: panel behaviour

GPUI's `WindowKind::PopUp` creates its own `GPUIPanel` (an `NSPanel` subclass, `gpui-pre-macos-0.3.9/src/window.rs:130`) with `NSWindowStyleMaskNonactivatingPanel`, level `NSPopUpWindowLevel` (101), and collection behaviour `CanJoinAllSpaces | FullScreenAuxiliary` (lines ~1021-1241). So **no class swap or objc hacks are needed** for: not stealing focus, floating over other apps, all Spaces, over fullscreen apps. (The Tauri app has to build all of this by hand with a KVO subclass; see the Tauri guide.) Use it for dropdown, floating button and overlay.

Fallback if a Mac shows the panel pulling the user off a fullscreen Space (documented in `tray.rs`, untested): Accessory activation policy at launch, Regular in `show_main`.

### 3.11 `floating.rs`: floating button -> panel, native move

GPUI can resize but **cannot move a window**; set the NSWindow frame natively via the view handle:

```rust
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    unsafe { h.ns_view.cast::<NSView>().as_ref() }.window()      // GPUI's AppKit handle is its live content view
}
```
- One window, two sizes (`BUTTON 64x64`, `PANEL 340x500`, transparent margin included). `resize()` keeps the **bottom-right corner fixed** and `fit()`s to `screen.visibleFrame()`. The last corner per slot is kept in `Wins.anchors` so toggling off/on, or collapse after drag, returns to the same spot. `ns.setHasShadow(false)` (system shadow around a transparent window looks wrong; commit 69cd7ef).
- Drag vs click: `Drag{begin,drag,end}` reads `NSEvent::mouseLocation()` (screen coordinates, so moving the window does not skew deltas); movement < 4pt is a click. `draggable(el, cx, |v| &mut v.drag, Self::moved)` wires mouse down/move/up; the click handler checks `!this.drag.moved`.
- `sync(cx)` reconciles open windows with `Settings` (button per display slot via `cx.displays()`, debug overlay), pruning dead handles with `handle.update(cx, |_,_,_| ()).is_ok()`. Displays are read at launch/setting change only (documented limit).
- Visual state to copy: ring colour = mode (active/paused/idle), badge = mode letter + remaining, dimmed (0.55 opacity) when offline.

### 3.12 Env-gated screenshot hooks

CI cannot click. Add env vars that put the app in each state at launch; keep them undocumented for users **[Copycat names]**:
`COPYCAT_GPUI_SCREEN=history|session|bindings|settings`, `COPYCAT_GPUI_DROPDOWN=1` (opens at `point(40,40)` of the main display 2s after launch via `cx.spawn` + timer, so main-window activation cannot blur it shut), `COPYCAT_GPUI_FLOATING=button|panel`, `COPYCAT_GPUI_DEBUG=1`. Screenshot runs use a taller window (980) so a full screen fits.

## 4. Daemon pieces the GUI depends on

### 4.1 `crates/<app>-launcher` (shared by both front ends)

Public: `socket_path()`, `ensure_daemon()`, `restart_daemon()` (DaemonStop, wait up to 3s, ensure), `find_system_copycatd()`, `version_at_least(have, want)`. Behaviour (read the file, ~225 lines incl. tests; `ensure_daemon`/`restart_daemon` block up to 4s/3s+4s, so call them only via `background_spawn`):
1. `ensure_daemon`: if a daemon answers and `daemon_version >= protocol::VERSION` (semver, see below), leave it. A *compatible* daemon is **not** stopped on quit: it is a shared background service.
2. If incompatible (or unreadable version), stop it **only if** a replacement exists (`find_bundled()` or `compatible_installed()`); otherwise leave the user's only daemon alone.
3. Start order `find_copycatd()`: compatible installed -> bundled sidecar -> any system copy -> `PATH` `copycatd`. **Installed preferred over bundled**: the keychain ACL is bound to the binary that created the item, so switching daemons loses access to stored keys.
4. Spawn detached, stdio null (daemon keeps its own log), then wait up to 4s for the socket. The daemon's own socket-in-use guard makes racing clients harmless.
5. Semver precedence implemented by hand (`semver_cmp`): pre-release < release (`0.5.0-rc.5 < 0.5.0 < 0.6.0-rc.1`), numeric identifiers compare numerically (`rc.10 > rc.9`), `+build` ignored. Ignoring pre-release once let a stale rc daemon pass for the release. Unit-tested; keep the tests.
6. **Bump the MINOR version whenever any Action/ResultBody changes**: the GUI decides compatibility from `StatusReport.daemon_version`, so a new action on an unchanged version would be sent to an old daemon that lacks it.

### 4.2 Doctor / permission truth (macOS TCC)

- Permissions belong to the **responsible app**: `Copycat.app` when the app spawns the daemon, the terminal when started from one. The same daemon can work from a terminal and be refused under the app. Therefore the daemon must ask the OS itself: `crates/copycat-daemon/src/platform/mac_permissions.rs` links `AXIsProcessTrusted`, `CGPreflightPostEventAccess`, `CGPreflightListenEventAccess`, `CGRequestPostEventAccess`, and reports them through the doctor checks. Surface them in the GUI.
- An **active** `CGEventTap` that consumes keys, plus `CGEventPost` injection, need **Accessibility** (the Accessibility list), not Input Monitoring. A passive listen-only tap is a different grant, and `CGEventTapCreate` can return non-null without it, so "tap created" is not proof of events flowing; read the daemon debug log.
- The tap is created once; a refusal sticks. After the user grants, **restart the daemon** (`restart_daemon()`, run off the UI thread; the Settings screen has a button).
- GUI rule `permission_blocked()` **[Copycat]**: any doctor check in `{macos-permissions, paste-injection, paste-interception, global-hotkeys}` with status Unavailable whose detail contains "accessibility", "input monitoring" or "event tap" shows the callout with the Settings panes.
- `MACOSX_DEPLOYMENT_TARGET=10.15` in release workflows: the `CGPreflight*` symbols exist from 10.15; lower targets would not load.

## 5. macOS-specific summary

| Concern | Do |
|---|---|
| Stay alive in the menu bar | `QuitMode::Explicit` + `on_reopen` + Quit menu item calling `cx.quit()` |
| Tray icon | Template PNG (black+alpha), `with_icon_as_template(true)`, global keepalive, main thread |
| Panels | `WindowKind::PopUp` (GPUIPanel): non-activating, all Spaces, fullscreen aux |
| Window moves | `objc2` NSWindow `setFrameOrigin` / `setFrame_display` (GPUI cannot) |
| Focus before paste | `NSApplication::deactivate()` + 150ms settle |
| Multi-display | `NSScreenNumber` == GPUI `DisplayId`; `cx.displays()`, `cx.primary_display()` |
| Permissions | Section 4.2 |
| Signing | Section 8: one stable cert for daemon, CLI, app |
| Untested (README) | paste landing in previous app; fullscreen without Space switch; second click closing cleanly |

Linux/Windows: GUI builds there are not done. Gate every macOS module with `#[cfg(target_os = "macos")]` in `main.rs` (and keep a `LastWindowClosed` fallback) so the crate still type-checks elsewhere.

## 6. Verification workflow (agent cannot see a screen)

Layers, cheapest first:
1. **Pure logic tests** (`cargo test --manifest-path apps/<app>-gpui/Cargo.toml`): settings parse, semver, anything with no window. Put logic in functions that take plain data.
2. **Local type-check on Linux** (the only thing you can do on the dev box). GPUI needs Vulkan/Wayland/xkbcommon on Linux and Metal on macOS, so use `docs/guides/tools/mac-check-gpui.sh <app dir>`: it clones the `gpui-pre-apple` crate from the cargo registry (run `cargo fetch` first), replaces its `build.rs` with one that writes empty shader outputs, patches it in with `--config patch.crates-io...`, and runs `cargo clippy --target aarch64-apple-darwin -D warnings` with stub `cc`/`ar` (`tools/fake-cc.sh`, `tools/fake-ar.sh`). Stage `Cargo.lock` changes first; the script restores the lock afterwards. A green run proves types, not behaviour.
3. **CI on macos-14** (`.github/workflows/gpui.yml`): `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, then screenshots. Triggered on pushes to the branch touching `apps/<app>-gpui/**`, `crates/**`, or the workflow; plus `workflow_dispatch`.
4. **Real daemon + seeded state in CI** (this is what makes screenshots meaningful). Pattern from `gpui.yml`:

```yaml
- run: cargo build -p copycat-daemon -p copycat-cli
- name: Screenshots
  run: |
    clip="$RUNNER_TEMP/clipboard.txt"; shots="$RUNNER_TEMP/shots"; mkdir -p "$shots"
    ./target/debug/copycatd --clipboard file --clipboard-file "$clip" --log-file "$RUNNER_TEMP/copycatd.log" &
    sleep 2
    printf '%s' "some text" > "$clip"; sleep 0.7        # seed state by rewriting the file (daemon polls 250ms)
    cc=./target/debug/copycat; $cc stack start; $cc paste next --no-inject
    app=apps/copycat-gpui/target/debug/copycat-gpui; status=0
    shot() { env "$1" $app & pid=$!; sleep "$3"; screencapture -x "$shots/$2.png"; kill $pid || status=1; wait $pid || true; sleep 1; }
    shot COPYCAT_GPUI_SCREEN=history history 8
    shot COPYCAT_GPUI_DROPDOWN=1 dropdown 10
    cp "$RUNNER_TEMP/copycatd.log" "$shots/"; exit $status    # keep shooting after a crash, fail at the end
- uses: actions/upload-artifact@v4
  if: always()
  with: { name: gpui-screens, path: "${{ runner.temp }}/shots" }
```
5. **Read the PNGs.** Download the artifact (`gh run download <id> -n gpui-screens -D /tmp/shots`, watch with `gh run watch <id> --exit-status` **without piping into `tail`**, a pipe masks the exit status), open each PNG with the Read tool, and compare against reference screenshots rendered from the other UI (Tauri HTML with a mock daemon) at the same size. Fix spacing/line-height/missing glyphs from the diff (commit 088c94c is a worked example). A blank shot = crash: read the `copycatd.log` and re-run the binary with a backtrace.
6. **Real-machine behaviour** (focus, Spaces, fullscreen, tray clicks, permissions) cannot be derived by reasoning. Add `eprintln!` diagnostics at each decision (click received, debounce hit, display id, computed origin, window handle alive), cut an `-rc`, and ask the user to run the binary **from a terminal** and paste output plus the daemon log (`copycat logs` or `copycatd --log debug`; at debug the macOS tap logs every key: absence = OS not delivering, presence = bug downstream). The one-line log `invalid mode 'kCFRunLoopCommonModes' provided to CFRunLoopRunSpecific` was the real cause of a "paste does nothing" bug after three wrong permission theories.

## 7. CI summary

- `gpui.yml`: macos-14, `dtolnay/rust-toolchain@stable` (clippy), `Swatinem/rust-cache@v2` with `workspaces: apps/copycat-gpui` and `.` (two workspaces, one `key: gpui`).
- Root CI must never build the GUI crate (excluded). The Tauri CI job needs a **stand-in sidecar** (`touch apps/copycat-gui/src-tauri/binaries/copycatd-aarch64-apple-darwin; chmod +x`) because `tauri-build` refuses to run without the externalBin on disk. The GPUI app has no sidecar, so no stand-in until you add one.

## 8. Not done in Copycat's GPUI app, and how to add it

Missing on `gpui`: packaging (.app/.dmg), signing, a bundled daemon, tray/Linux/Windows. Mirror the Tauri pipeline (`.github/workflows/gui-release.yml`, `.github/actions/macos-signing/action.yml`, `release.yml`):

1. **Tag namespaces**: `v*` -> daemon/CLI release (`release.yml`); `gui-v*` -> app (`gui-release.yml`), so the pipelines never fire on each other. Pre-release with `-rc.N` for any platform code not run on a real display. `install.sh` **skips pre-releases**: pin `COPYCAT_VERSION=<tag>` to install one.
2. **Bundle**: build `.app` with `cargo-bundle` or a scripted `Info.plist` (unverified suggestion: Copycat's GPUI app has no bundling; the Tauri pipeline uses `tauri-action`). Put the daemon at `Contents/MacOS/copycatd` (the launcher's `find_bundled()` looks beside the exe; an **empty file counts as a stand-in, not a daemon**).
3. **Universal binary**: build both targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`; arm64 runner cross-builds x86_64) and `lipo -create`. If a Tauri-style sidecar scheme is used you need **three names**: per-arch sidecars (`copycatd-aarch64-apple-darwin`, `copycatd-x86_64-apple-darwin`) for each compile, plus a lipo'd `copycatd-universal-apple-darwin` for the final copy (commits f7c0fca, 8b85600).
4. **Signing, every build**: composite action `.github/actions/macos-signing` imports the base64 p12 from secrets `MACOS_CERTIFICATE` / `MACOS_CERTIFICATE_PASSWORD` into a temp keychain, trusts it for codesign on the runner, and outputs `identity` (`Copycat Code Signing`, or `-` ad hoc when the secret is absent, with a warning). Then `codesign --force --sign "$IDENTITY"` the daemon, the CLI **and** the app; afterwards, when `$IDENTITY != "-"`, verify `codesign -dvv ... | grep -q "Authority=$IDENTITY"` so a build that silently fell back to ad hoc is a red run (both release workflows skip the check for the ad-hoc case). Why: TCC and keychain grants key on code identity; an ad-hoc signature changes every build, so grants go stale. **Never rotate the cert casually**; a rotation means every user must `tccutil reset Accessibility <bundle id>`, re-grant, and restart the daemon (the release notes of gui-v0.1.0-rc.13 say exactly this).
5. Set `MACOSX_DEPLOYMENT_TARGET: "10.15"` at workflow `env`.
6. Self-signed means not notarized: release notes must give the first-launch workaround (`xattr -dr com.apple.quarantine /Applications/<App>.app`, or right-click Open).
7. Download the artifact once on a real Mac before announcing; a release with unverified UI is `-rc`.

## 9. Pitfalls we hit (symptom -> cause -> fix)

| Symptom | Cause | Fix |
|---|---|---|
| Debug build/CI panics on first list row | Two `.hover()` on one element | One hover per element; pass hover as parameter (088c94c) |
| Panic when dropping a row | `clamp(cursor, last)` with cursor > last | `.min(last).max(cursor)`; daemon rejects stale move by id (9171ba1) |
| All icons blank | No asset source | `.with_assets(gpui_kit::assets::Assets)` |
| Logo is a flat single-colour blob | `svg()` is a mask | `img(Arc<Image>)` from `ImageFormat::Svg` |
| Tray icon vanishes after startup | `TrayIcon` dropped | Store in a Global |
| Tray handler can't touch GPUI | Runs on AppKit thread, no `&mut App` | Post to `async_channel`, drain in `cx.spawn` |
| Click toggles twice / dropdown reopens as it closes | Down+Up events; blur-before-release | Act on `Up` only; 300ms `blurred_at` debounce |
| Dropdown closes immediately | Window resigns key while opening | Close on blur only after `was_active` |
| Dropdown on wrong screen/offset on Retina | Used tray-icon rect | Status item window frame + `NSScreenNumber` |
| Floating button steals focus / not over fullscreen | Plain window | `WindowKind::PopUp`, `focus: false` |
| Can't move/anchor a window | GPUI has no move API | `NSWindow.setFrameOrigin` via `raw-window-handle` |
| Paste lands in our own window | Clicking made our app active | `NSApplication::deactivate()` then 150ms before the daemon injects |
| App quits when main window closes | Default quit mode | `QuitMode::Explicit` + `on_reopen` |
| Fonts differ from design | Not on macOS | Bundle TTFs + `add_fonts`; no letter-spacing: `spaced()` |
| App feels like a webapp | Screens drawn from `div()` + hex + glyphs | Real widgets + theme tokens (section 3.4) |
| Pin star looks the same both states | Font lacks glyph, falls back | Colour carries state |
| Transparent popup shows square shadow | System shadow | `setHasShadow(false)` + own `BoxShadow` (69cd7ef) |
| Stale list shown under a new session | Reply for an ended session | Tag with session id, drop mismatches |
| Items never reload | In-flight flag not cleared on error | Use `request` directly and clear in callback |
| Permission granted but nothing works | Tap created once, refusal sticks | Restart daemon after grant |
| Works from terminal, fails under app | TCC attributes to responsible app | Doctor asks OS; test via the app launch |
| Grants vanish after each update | Ad-hoc signature changes every build | One stable cert on every binary; never rotate |
| Old rc daemon accepted as release | Pre-release ignored in compare | Hand-written semver precedence + tests |
| New action fails on "compatible" daemon | Version not bumped | Bump MINOR on any Action change |
| Keychain items inaccessible after daemon swap | ACL bound to creating binary | Prefer installed daemon over bundled when compatible |
| Universal bundle fails to find sidecar | Tauri needs per-arch AND universal names | Provide all three (8b85600) |
| Tauri CI compile fails without sidecar | `tauri-build` needs externalBin on disk | Touch a stand-in file in CI |
| CI looks green but run failed | `gh run watch ... \| tail` masks status | `gh run watch <id> --exit-status` unpiped |
| `install.sh` installs the old version | Skips pre-releases | `COPYCAT_VERSION=<tag>` |
| macOS key events never arrive, no error | Run loop run in `kCFRunLoopCommonModes` | `CFRunLoop::run_current()` (default mode), add source to common set |
| Linux CI can't build GUI | Vulkan/Wayland/xkbcommon | GUI is its own workspace, built on macos-14 only |

## 10. Acceptance checklist

- [ ] `grep gpui Cargo.toml` shows only `gpui-kit = "=x.y.z"`; app has its own `[workspace]` and is in root `exclude`.
- [ ] Root `cargo build/test/clippy` pass without GPUI headers; `cargo clippy --manifest-path apps/<app>-gpui/Cargo.toml --all-targets -- -D warnings` passes on macos-14.
- [ ] No socket call on the UI thread (`grep "call(" src` appears only inside `background_spawn`/`request`).
- [ ] Exactly one poll task; all views `observe` the `Daemon` entity.
- [ ] Launcher semver tests pass; protocol `VERSION` = package version; minor bumped if any Action changed.
- [ ] Tray icon: global keepalive, `Up`-only, debounce, template image, Open/Quit menu.
- [ ] Dropdown opens under the icon on the clicked display, closes on blur, paste lands in previous app (real-machine report received).
- [ ] Floating button never takes focus, shows over a fullscreen app, drags without triggering a click, remembers its position.
- [ ] Main window closing leaves the app in the menu bar; Dock click reopens it.
- [ ] `gui.json` round-trips; bad file = defaults; toggles open/close windows live.
- [ ] Every screen/popup has an env-gated screenshot in CI; PNGs were read and compared to the reference; no blank shot.
- [ ] Doctor reports the OS-asked permissions; GUI callout + restart-daemon button.
- [ ] All binaries signed with the one cert; `Authority=` verified in CI; deployment target 10.15.
- [ ] Release tagged in the right namespace; `-rc` if any platform behaviour unverified; release notes list the first-launch workaround and permission steps.
- [ ] Commit messages plain, no AI trailers.

For the floating window's rendering fixes see [gpui-floating-window-fixes.md](gpui-floating-window-fixes.md).
