//! The Copycat desktop shell.
//!
//! This is deliberately thin: the daemon owns every bit of clipboard state and
//! speaks it over a local socket, so the GUI is one more client of that socket
//! (ADR-003). The frontend renders; this Rust layer does three things — resolve
//! the socket, forward a request to the daemon, and poll the daemon's state so
//! the panel stays live. No clipboard logic lives here.

use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use copycat_launcher::{ensure_daemon, find_system_copycatd, socket_path};
use copycat_protocol::{Action, Request, call, request};
use tauri::{
    Emitter, LogicalSize, Manager, PhysicalPosition, Rect, WebviewWindow, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// One command the whole frontend goes through: name an action, hand it args,
/// get the daemon's reply back as JSON.
///
/// The request is built as the wire envelope and parsed by the protocol's own
/// tolerant deserializer, so "no arguments" works whether the frontend sends
/// `null`, `{}`, or nothing — the same latitude a config binding gets.
///
/// Async so the blocking socket round trip runs off the main thread — and so a
/// paste can wait for focus to move without freezing every window.
#[tauri::command]
async fn daemon(
    app: tauri::AppHandle,
    action: String,
    args: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("version".into(), serde_json::json!(1));
    envelope.insert("id".into(), serde_json::json!("gui"));
    envelope.insert("action".into(), serde_json::Value::String(action));
    if let Some(args) = args {
        envelope.insert("args".into(), args);
    }

    let req: Request = serde_json::from_value(serde_json::Value::Object(envelope))
        .map_err(|e| format!("bad request: {e}"))?;

    if injects_paste(&req.action) {
        hand_focus_back(&app);
    }

    request(&socket_path(), &req)
        .map(|body| serde_json::to_value(body).unwrap_or(serde_json::Value::Null))
        // The daemon's message is already written for a person; pass it through.
        .map_err(|error| error.message)
}

/// Whether an action ends with the daemon pressing the paste chord for the user.
fn injects_paste(action: &Action) -> bool {
    matches!(
        action,
        Action::PasteLatest { .. }
            | Action::PasteOffset { .. }
            | Action::PasteId { .. }
            | Action::PasteNext { peek: false }
            | Action::PasteMode { inject: true }
            | Action::GroupPaste
            | Action::GroupPasteLast { .. }
    )
}

/// Give focus back to the app the user was in, before the daemon pastes.
///
/// Clicking any Copycat window — the menu-bar panel, the floating button, the
/// main window — makes Copycat the active app, so the paste chord the daemon
/// injects would land in Copycat instead of where the user is working.
/// Deactivating hands focus back to the previous app; the floating windows stay
/// on screen, since they sit above every app anyway.
fn hand_focus_back(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(|| {
            if let Some(mtm) = objc2::MainThreadMarker::new() {
                objc2_app_kit::NSApplication::sharedApplication(mtm).deactivate();
            }
        });
        // ponytail: a fixed settle delay for the other app to take focus; wait
        // on NSWorkspace's activation notification if 150ms proves flaky.
        std::thread::sleep(Duration::from_millis(150));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// The floating window's two sizes (logical px, including the transparent
/// margin its shadow draws into): a round button, and the panel it opens into.
const FLOATING_BUTTON: (f64, f64) = (64.0, 64.0);
const FLOATING_PANEL: (f64, f64) = (340.0, 500.0);

/// Show or hide the floating button: on the primary screen, or one on every
/// screen. A button is parked at the bottom right of its screen the first time
/// it shows; after that it stays wherever it was dragged.
///
/// Async, so the extra windows are built off the main thread (building one on
/// it can deadlock on Windows); what touches AppKit runs back on it.
#[tauri::command(async)]
fn set_floating(app: tauri::AppHandle, on: bool, every_screen: Option<bool>) {
    let primary = app.primary_monitor().ok().flatten();
    // The other screens, in a stable order: button N lives on the Nth.
    // ponytail: applied when toggled or at launch; plugging in a screen later
    // needs a re-toggle until monitor-change events are wired in.
    let others: Vec<tauri::Monitor> = if on && every_screen.unwrap_or(false) {
        app.available_monitors()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| primary.as_ref().map(|p| p.position()) != Some(m.position()))
            .collect()
    } else {
        Vec::new()
    };

    let mut fresh = Vec::new();
    for n in 1..=others.len() {
        let label = format!("floating-{n}");
        if app.get_webview_window(&label).is_none() && floating_window(&app, &label).is_ok() {
            fresh.push(n);
        }
    }

    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        static PLACED: AtomicBool = AtomicBool::new(false);
        for n in fresh {
            if let Some(window) = handle.get_webview_window(&format!("floating-{n}")) {
                float_everywhere(&window, false);
                park_bottom_right(&window, &others[n - 1]);
            }
        }
        for (label, window) in handle.webview_windows() {
            let n = match label.as_str() {
                "floating" => 0,
                extra => match extra.strip_prefix("floating-").and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => continue,
                },
            };
            if !on || n > others.len() {
                // Hidden, never closed: tearing down a swapped panel throws.
                let _ = window.hide();
                continue;
            }
            if n == 0 && !PLACED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                if let Some(primary) = &primary {
                    park_bottom_right(&window, primary);
                }
            }
            show_over(&window);
        }
    });
}

/// Another floating button, built like the one in `tauri.conf.json`.
fn floating_window(app: &tauri::AppHandle, label: &str) -> tauri::Result<WebviewWindow> {
    tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App("floating.html".into()))
        .title("Copycat")
        .inner_size(FLOATING_BUTTON.0, FLOATING_BUTTON.1)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .visible(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .accept_first_mouse(true)
        .disable_drag_drop_handler()
        .build()
}

/// Put a floating button in the bottom right of a screen's work area.
///
/// Worked out on the placement grid (see [`unit`]): a fresh button sits on
/// the primary screen, so its own scale says nothing about the target's.
fn park_bottom_right(window: &WebviewWindow, monitor: &tauri::Monitor) {
    let u = unit(monitor.scale_factor());
    let pt = monitor.scale_factor() / u; // one point on the placement grid
    let area = monitor.work_area();
    let right = (area.position.x as f64 + area.size.width as f64) / u;
    let bottom = (area.position.y as f64 + area.size.height as f64) / u;
    let x = right - (FLOATING_BUTTON.0 + 24.0) * pt;
    let y = bottom - (FLOATING_BUTTON.1 + 24.0) * pt;
    let _ = if cfg!(target_os = "macos") {
        window.set_position(tauri::LogicalPosition::new(x, y))
    } else {
        window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    };
}

/// Grow the floating button into its panel, or shrink it back.
///
/// The bottom-right corner stays put, so the button does not jump when the
/// panel closes; if growing pushed the panel off an edge, it is pulled back
/// onto the display it is on.
#[tauri::command]
fn floating_expand(window: WebviewWindow, expanded: bool) {
    let (w, h) = if expanded { FLOATING_PANEL } else { FLOATING_BUTTON };
    let scale = window.scale_factor().unwrap_or(1.0);
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.outer_size()) else { return };
    let (new_w, new_h) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let mut x = pos.x + size.width as i32 - new_w;
    let mut y = pos.y + size.height as i32 - new_h;
    if let Ok(Some(monitor)) = window.current_monitor() {
        let area = monitor.work_area();
        let max_x = area.position.x + area.size.width as i32 - new_w;
        let max_y = area.position.y + area.size.height as i32 - new_h;
        if max_x >= area.position.x {
            x = x.clamp(area.position.x, max_x);
        }
        if max_y >= area.position.y {
            y = y.clamp(area.position.y, max_y);
        }
    }
    let _ = window.set_size(LogicalSize::new(w, h));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Move the calling window by a pointer delta, in logical px.
///
/// The floating window drags itself this way rather than with the OS drag:
/// telling a click from a drag needs a movement threshold, and the OS drag can
/// only begin from the mouse-down, not partway through a gesture.
#[tauri::command]
fn move_window_by(window: WebviewWindow, dx: f64, dy: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Ok(pos) = window.outer_position() {
        let _ = window.set_position(PhysicalPosition::new(
            pos.x + (dx * scale).round() as i32,
            pos.y + (dy * scale).round() as i32,
        ));
    }
}

/// Open the full window (history, session, bindings, settings) and tuck the
/// menu-bar panel away.
#[tauri::command]
fn open_main(app: tauri::AppHandle) {
    // A Dock icon and Cmd-Tab while the main window is open (see `run`).
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
}

/// Open a macOS Privacy & Security pane, so the user does not have to hunt for
/// it. The daemon runs as this app's child, so its permissions are attributed
/// to Copycat.app — which is what the user must grant here.
#[tauri::command]
fn open_settings(pane: String) {
    let url = match pane.as_str() {
        "input-monitoring" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
        }
        // Accessibility — what the daemon's event tap (hotkeys, leader, the
        // paste chord) and paste typing need.
        _ => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    };
    let _ = Command::new("open").arg(url).spawn();
}

/// Show or hide the debug overlay — a small always-on-top window that shows the
/// live mode, the next item, and each binding trigger as it fires.
#[tauri::command]
fn set_debug_overlay(app: tauri::AppHandle, on: bool) {
    if let Some(window) = app.get_webview_window("debug") {
        if on {
            position_overlay(&window);
            show_over(&window);
        } else {
            let _ = window.hide();
        }
    }
}

/// Park the overlay in the top-right of the primary display's work area.
fn position_overlay(window: &WebviewWindow) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let w = window.outer_size().map(|s| s.width as i32).unwrap_or(600);
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let area = monitor.work_area();
        let margin = (16.0 * scale) as i32;
        let x = area.position.x + area.size.width as i32 - w - margin;
        let y = area.position.y + margin;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

/// Restart the daemon so a freshly granted permission takes effect.
/// Async, so its waiting happens off the main thread and the UI stays live.
#[tauri::command(async)]
fn restart_daemon() {
    copycat_launcher::restart_daemon();
}

/// Show a window on every Space, a fullscreen app's included.
///
/// Joining all Spaces and being fullscreen-auxiliary is not enough on macOS:
/// the window server keeps an ordinary window off another app's fullscreen
/// Space, and showing one activates Copycat, which drags the user off it. A
/// non-activating panel is let in, so the window becomes one: its class is
/// swapped for an NSPanel subclass (tao builds every window as a plain
/// NSWindow) and the non-activating bit is added. `takes_keys` is for the
/// dropdown, whose search box needs the keyboard; the others never take focus.
fn float_everywhere(window: &WebviewWindow, takes_keys: bool) {
    let _ = window.set_visible_on_all_workspaces(true);
    #[cfg(target_os = "macos")]
    if let Ok(ptr) = window.ns_window() {
        // Called from `setup`, on the main thread, while the window is alive.
        mac::make_panel(unsafe { &*ptr.cast() }, takes_keys);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = takes_keys;
}

/// Show a window without activating Copycat.
///
/// `set_focus` activates the app on macOS, and an activation pulls the user
/// off a fullscreen app's Space; a panel orders itself front instead, and
/// becomes key without activating anything if it takes keys.
fn show_over(window: &WebviewWindow) {
    let _ = window.show();
    #[cfg(target_os = "macos")]
    if let (Ok(ptr), Some(_)) = (window.ns_window(), objc2::MainThreadMarker::new()) {
        let ns_window: &objc2_app_kit::NSWindow = unsafe { &*ptr.cast() };
        ns_window.orderFrontRegardless();
        if ns_window.canBecomeKeyWindow() {
            ns_window.makeKeyWindow();
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::{Mutex, MutexGuard};

    use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
    use objc2::{define_class, msg_send, sel, ClassType, MainThreadOnly};
    use objc2_app_kit::{
        NSPanel, NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
    };

    // No ivars and no Drop, so neither class is bigger than NSPanel: the
    // swap below only ever shrinks the object, which is what makes it sound.
    define_class!(
        #[unsafe(super(NSPanel))]
        #[thread_kind = MainThreadOnly]
        #[name = "CopycatKeyPanel"]
        struct KeyPanel;

        impl KeyPanel {
            // A borderless window refuses key status by default.
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key(&self) -> bool {
                true
            }
            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main(&self) -> bool {
                false
            }
        }
    );

    define_class!(
        #[unsafe(super(NSPanel))]
        #[thread_kind = MainThreadOnly]
        #[name = "CopycatPassivePanel"]
        struct PassivePanel;

        impl PassivePanel {
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key(&self) -> bool {
                false
            }
            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main(&self) -> bool {
                false
            }
        }
    );

    /// Each swapped window and the class it had, so quitting can put it back.
    static SWAPPED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

    pub fn make_panel(ns_window: &NSWindow, takes_keys: bool) {
        // Only ever swap tao's own class, or the KVO subclass AppKit makes of
        // it once something observes the window (on a real Mac, always).
        let class = AnyObject::class(ns_window);
        if class.name() != c"TaoWindow" && class.name() != c"NSKVONotifying_TaoWindow" {
            eprintln!("copycat: not making {class:?} a panel; it stays off fullscreen Spaces");
            return;
        }
        let panel = if takes_keys { KeyPanel::class() } else { PassivePanel::class() };
        // Not `AnyObject::set_class`: it debug-asserts equal sizes, and
        // TaoWindow is one ivar bigger. Never call tao's `set_focusable` on
        // these windows afterwards — that ivar is gone, and tao panics.
        let object = ns_window as *const NSWindow as *mut AnyObject;
        lock(&SWAPPED).push((object as usize, class as *const AnyClass as usize));
        unsafe {
            objc2::ffi::object_setClass(object, panel);
        }

        // An Objective-C exception would abort the process, so a refused
        // style mask is logged rather than fatal.
        let made = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            ns_window.setStyleMask(ns_window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
            // The swap skipped NSPanel's init, and setStyleMask: alone does
            // not tell the window server; this private call does. It is
            // checked for, so a macOS without it just skips it.
            let prevents = sel!(_setPreventsActivation:);
            if ns_window.respondsToSelector(prevents) {
                let _: () = unsafe { msg_send![ns_window, _setPreventsActivation: true] };
            }
        }));
        if let Err(error) = made {
            eprintln!("copycat: macOS refused the non-activating panel style: {error:?}");
        }

        ns_window.setHidesOnDeactivate(false);
        ns_window.setCollectionBehavior(
            ns_window.collectionBehavior()
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::CanJoinAllApplications,
        );
        // Above the menu bar a fullscreen Space reveals (24); tao's
        // `alwaysOnTop` level is 5. Last, since NSPanel setters reset it.
        // ponytail: 25 per Ganymede/typelite on macOS 26; Apple DTS says 1000
        // (NSScreenSaverWindowLevel) — raise it if the log shows the window
        // is still not on the active Space.
        ns_window.setLevel(NSStatusWindowLevel);
        eprintln!(
            "copycat: panel {:?} level {} behavior {:#x} style {:#x}",
            AnyObject::class(ns_window),
            ns_window.level(),
            ns_window.collectionBehavior().bits(),
            ns_window.styleMask().bits(),
        );
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Give every swapped window its original class back.
    ///
    /// Tearing down a window whose KVO subclass was swapped away throws an
    /// Objective-C exception, which turns quitting into a crash; restored, it
    /// tears down as tao built it. Main thread, at exit, while the windows
    /// still exist.
    pub fn restore_classes() {
        for (object, class) in lock(&SWAPPED).drain(..) {
            unsafe {
                objc2::ffi::object_setClass(object as *mut AnyObject, &*(class as *const AnyClass));
            }
        }
    }
}

/// Drop the panel under the tray icon, on the screen the icon is on.
///
/// The tray click event carries the icon's on-screen rectangle; the panel's
/// horizontal centre is aligned to the icon's and its top sits just under the
/// menu bar. Without this the window opens wherever the OS last left it.
fn position_under_tray(window: &WebviewWindow, rect: Rect) {
    let icon_pos = rect.position.to_physical::<f64>(1.0);
    let icon_size = rect.size.to_physical::<f64>(1.0);
    let screens: Vec<Screen> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| Screen {
            position: (m.position().x as f64, m.position().y as f64),
            size: (m.size().width as f64, m.size().height as f64),
            work_position: (m.work_area().position.x as f64, m.work_area().position.y as f64),
            work_size: (m.work_area().size.width as f64, m.work_area().size.height as f64),
            scale: m.scale_factor(),
        })
        .collect();
    // The pointer is on the icon at click time, and unlike the icon's
    // rectangle it is reported with one fixed scale (the primary screen's), so
    // it says unambiguously which screen was clicked.
    let primary_scale = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    let pointer = window
        .cursor_position()
        .ok()
        .map(|p| (p.x / unit(primary_scale), p.y / unit(primary_scale)));
    let window_scale = window.scale_factor().unwrap_or(1.0);
    let window_size = window
        .outer_size()
        .map(|s| (s.width as f64 / unit(window_scale), s.height as f64 / unit(window_scale)))
        .unwrap_or((392.0, 588.0));

    let Some((x, y)) = place_under_icon(
        (icon_pos.x, icon_pos.y, icon_size.width, icon_size.height),
        pointer,
        &screens,
        window_size,
    ) else {
        return;
    };
    let _ = if cfg!(target_os = "macos") {
        window.set_position(tauri::LogicalPosition::new(x, y))
    } else {
        window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    };
}

/// One display, with every rectangle exactly as Tauri reports it.
#[derive(Debug, Clone, Copy)]
struct Screen {
    position: (f64, f64),
    size: (f64, f64),
    work_position: (f64, f64),
    work_size: (f64, f64),
    scale: f64,
}

/// What a reported "physical" value is in units of the grid windows are
/// placed on.
///
/// On macOS every rectangle (a screen's, the tray icon's) comes back as
/// points × *that screen's own* scale, so a 2x laptop and a 1x external
/// display report numbers on two different grids. Mixing them is what dropped
/// the panel on the wrong screen or far off the icon. Dividing by the scale
/// each value was made with gets back to AppKit's one global grid of points.
/// Windows and Linux already report one global grid of pixels.
fn unit(scale: f64) -> f64 {
    if cfg!(target_os = "macos") { scale } else { 1.0 }
}

/// Where the panel's top-left goes, on the placement grid (see [`unit`]), or
/// `None` when no screen holds the icon.
///
/// `icon` is the tray rectangle as reported (x, y, w, h); `pointer` and
/// `window` (the panel's size) are already on the placement grid.
fn place_under_icon(
    icon: (f64, f64, f64, f64),
    pointer: Option<(f64, f64)>,
    screens: &[Screen],
    window: (f64, f64),
) -> Option<(f64, f64)> {
    let (ix, iy, iw, ih) = icon;
    let contains = |s: &Screen, (x, y): (f64, f64)| {
        let u = unit(s.scale);
        let (sx, sy) = (s.position.0 / u, s.position.1 / u);
        let (sw, sh) = (s.size.0 / u, s.size.1 / u);
        x >= sx && x < sx + sw && y >= sy && y < sy + sh
    };
    // The pointer picks the screen. Without one, guess from the icon: its
    // rectangle was made with its own screen's scale, which on macOS can match
    // more than one screen.
    let screen = pointer
        .and_then(|p| screens.iter().find(|s| contains(s, p)))
        .or_else(|| screens.iter().find(|s| contains(s, (ix / unit(s.scale), iy / unit(s.scale)))))?;
    let u = unit(screen.scale);
    let pt = screen.scale / u; // one point on the placement grid
    let (ix, iy, iw, ih) = (ix / u, iy / u, iw / u, ih / u);
    let (ax, ay) = (screen.work_position.0 / u, screen.work_position.1 / u);
    let (aw, ah) = (screen.work_size.0 / u, screen.work_size.1 / u);
    let (w, h) = window;
    let margin = 8.0 * pt;

    let x = (ix + iw / 2.0 - w / 2.0).max(ax + margin).min((ax + aw - w - margin).max(ax + margin));
    // Under the icon, but kept on the screen: a bottom taskbar (Windows) puts
    // the icon at the bottom, so the panel goes above it instead.
    let y = (iy + ih + 2.0 * pt).max(ay).min((ay + ah - h - margin).max(ay));
    Some((x, y))
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            daemon,
            open_main,
            open_settings,
            restart_daemon,
            set_debug_overlay,
            set_floating,
            floating_expand,
            move_window_by
        ])
        .setup(|app| {
            // Right-click menu: the escape hatches that must always work.
            let open = MenuItem::with_id(app, "open", "Open Copycat", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Copycat", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;

            // One tray icon. Left-click toggles the panel; right-click drops the
            // menu. On macOS it is a template image — the brand mark in black and
            // alpha (copycat-package/brand/selected/copycat-menubar-template.svg)
            // — which the system tints for a light or dark menu bar, like every
            // built-in menu extra. Nothing tints it elsewhere, so other platforms
            // get the coloured app icon.
            #[cfg(target_os = "macos")]
            let (icon, template) = (
                tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?,
                true,
            );
            #[cfg(not(target_os = "macos"))]
            let (icon, template) = (
                app.default_window_icon()
                    .cloned()
                    .ok_or("no default window icon to use for the tray")?,
                false,
            );

            TrayIconBuilder::with_id("copycat")
                .icon(icon)
                .icon_as_template(template)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Copycat")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => open_main(app.clone()),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        if let Some(window) = tray.app_handle().get_webview_window("panel") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                position_under_tray(&window, rect);
                                show_over(&window);
                                // Never on macOS: it activates Copycat.
                                #[cfg(not(target_os = "macos"))]
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Close the panel when it loses focus, the way a menu-bar dropdown
            // does — click anywhere else and it goes away.
            if let Some(panel) = app.get_webview_window("panel") {
                float_everywhere(&panel, true);
                let hide_target = panel.clone();
                panel.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        let _ = hide_target.hide();
                    }
                });
            }

            // The overlay and the floating button float over everything,
            // including fullscreen apps.
            for label in ["debug", "floating"] {
                if let Some(window) = app.get_webview_window(label) {
                    float_everywhere(&window, false);
                }
            }

            // A menu-bar app: no Dock icon, and activating it does not pull
            // the user off a fullscreen app's Space onto the desktop. A
            // regular app's activation does, which made the panel vanish
            // there. The main window turns the Dock icon back on while it is
            // open (`open_main`).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Closing the main window hides it rather than quitting: this is a
            // menu-bar app and lives in the tray.
            if let Some(main) = app.get_webview_window("main") {
                let hide_target = main.clone();
                main.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = hide_target.hide();
                        #[cfg(target_os = "macos")]
                        let _ = hide_target
                            .app_handle()
                            .set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                });
            }

            // Poll the daemon and push its state to the panel. Polling, not a
            // subscription: it is what the TUI does, and a real event stream on
            // the protocol is a later optimization, not a v1 blocker.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                // Bring the daemon up before the first poll, so a fresh launch
                // connects instead of flashing "offline".
                ensure_daemon();
                loop {
                    // Whether the CLI tools are installed system-wide; drives the
                    // "install copycat" nudge when the app is on its bundled daemon.
                    let installed = find_system_copycatd().is_some();
                    let payload = match call(&socket_path(), Action::Status) {
                        Ok(body) => serde_json::json!({ "connected": true, "installed": installed, "status": body }),
                        Err(error) => serde_json::json!({ "connected": false, "installed": installed, "error": error.message }),
                    };
                    let _ = handle.emit("daemon-state", payload);
                    std::thread::sleep(Duration::from_millis(600));
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Copycat failed to start")
        .run(|_app, event| {
            // Only a real exit: `code: None` is the last window closing, and
            // this app keeps running in the tray after that.
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { code: Some(_), .. } | tauri::RunEvent::Exit
            ) {
                #[cfg(target_os = "macos")]
                mac::restore_classes();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_panel_lands_under_an_icon_on_a_second_screen_of_another_scale() {
        // A 2x laptop (1512x982 points) with a 1x display to its right
        // (1920x1080 points), both menu bars 25 points tall. Each rectangle is
        // reported as points times its own screen's scale.
        let laptop = Screen {
            position: (0.0, 0.0),
            size: (3024.0, 1964.0),
            work_position: (0.0, 50.0),
            work_size: (3024.0, 1914.0),
            scale: 2.0,
        };
        let external = Screen {
            position: (1512.0, 0.0),
            size: (1920.0, 1080.0),
            work_position: (1512.0, 25.0),
            work_size: (1920.0, 1055.0),
            scale: 1.0,
        };
        let screens = [laptop, external];

        // Icon at x=3000 points on the external display: reported at 1x. Read
        // at the laptop's 2x it would be x=1500, inside the laptop. The pointer
        // is what tells the two apart.
        let icon = (3000.0, 0.0, 30.0, 24.0);
        let (x, y) = place_under_icon(icon, Some((3010.0, 10.0)), &screens, (392.0, 588.0)).unwrap();
        assert_eq!((x, y), (2819.0, 26.0));

        // Icon near the laptop's right edge at x=1400 points: reported at 2x.
        // Clamped so the panel stays on the laptop.
        let icon = (2800.0, 0.0, 60.0, 48.0);
        let (x, y) = place_under_icon(icon, Some((1410.0, 10.0)), &screens, (392.0, 588.0)).unwrap();
        assert_eq!((x, y), (1112.0, 26.0));
    }

}
