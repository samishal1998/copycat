//! The Copycat desktop shell.
//!
//! This is deliberately thin: the daemon owns every bit of clipboard state and
//! speaks it over a local socket, so the GUI is one more client of that socket
//! (ADR-003). The frontend renders; this Rust layer does three things — resolve
//! the socket, forward a request to the daemon, and poll the daemon's state so
//! the panel stays live. No clipboard logic lives here.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use copycat_protocol::{Action, Request, call, default_socket_path, request};
use tauri::{
    Emitter, Manager, PhysicalPosition, Rect, WebviewWindow, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// Where the daemon listens. Shared with the CLI through the protocol crate, so
/// the GUI and CLI can never disagree about the path.
fn socket_path() -> PathBuf {
    default_socket_path().unwrap_or_else(|| PathBuf::from("copycat.sock"))
}

/// One command the whole frontend goes through: name an action, hand it args,
/// get the daemon's reply back as JSON.
///
/// The request is built as the wire envelope and parsed by the protocol's own
/// tolerant deserializer, so "no arguments" works whether the frontend sends
/// `null`, `{}`, or nothing — the same latitude a config binding gets.
#[tauri::command]
fn daemon(action: String, args: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("version".into(), serde_json::json!(1));
    envelope.insert("id".into(), serde_json::json!("gui"));
    envelope.insert("action".into(), serde_json::Value::String(action));
    if let Some(args) = args {
        envelope.insert("args".into(), args);
    }

    let req: Request = serde_json::from_value(serde_json::Value::Object(envelope))
        .map_err(|e| format!("bad request: {e}"))?;

    request(&socket_path(), &req)
        .map(|body| serde_json::to_value(body).unwrap_or(serde_json::Value::Null))
        // The daemon's message is already written for a person; pass it through.
        .map_err(|error| error.message)
}

/// Open the full window (history, session, bindings, settings) and tuck the
/// menu-bar panel away.
#[tauri::command]
fn open_main(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(panel) = app.get_webview_window("panel") {
        let _ = panel.hide();
    }
}

/// Start the daemon if it is not already running.
///
/// Opening the app and being told "daemon offline" is a poor first run, so the
/// GUI brings the daemon up itself. It does not stop it on quit — the daemon is
/// a background service the CLI and other clients share, and the GUI is one
/// client, not its owner.
fn ensure_daemon() {
    let socket = socket_path();
    if copycat_protocol::is_running(&socket) {
        return;
    }
    let Some(binary) = find_copycatd() else { return };

    // Detached, output discarded — the daemon keeps its own log file. Its own
    // socket-in-use guard makes a duplicate start harmless if two clients race.
    let _ = Command::new(binary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    // Give it a moment to bind before the first poll declares it offline.
    for _ in 0..40 {
        if copycat_protocol::is_running(&socket) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Locate the `copycatd` binary the CLI installer or a package manager placed.
///
/// A GUI launched from Finder has a minimal `PATH`, so the common install
/// locations are searched explicitly before falling back to the bare name.
fn find_copycatd() -> Option<PathBuf> {
    let name = "copycatd";

    // Beside the GUI binary first — where a bundled sidecar would live.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join(name);
            if beside.is_file() {
                return Some(beside);
            }
        }
    }

    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/bin").join(name)); // installer default
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(name)); // Homebrew (Apple silicon)
    candidates.push(PathBuf::from("/usr/local/bin").join(name)); // Homebrew (Intel) / manual
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Last resort: let the OS resolve it on PATH, if it happens to be there.
    Some(PathBuf::from(name))
}

/// Drop the panel under the tray icon, kept on the icon's monitor.
///
/// The tray click event carries the icon's on-screen rectangle; the panel's
/// horizontal centre is aligned to the icon's and its top sits just under the
/// menu bar. Without this the window opens wherever the OS last left it —
/// mid-screen — which is the bug this fixes.
fn position_under_tray(window: &WebviewWindow, rect: Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let icon_pos = rect.position.to_physical::<i32>(scale);
    let icon_size = rect.size.to_physical::<i32>(scale);
    let win_w = window.outer_size().map(|s| s.width as i32).unwrap_or(760);

    let mut x = icon_pos.x + icon_size.width / 2 - win_w / 2;
    let y = icon_pos.y + icon_size.height + (2.0 * scale) as i32;

    // Clamp to the working area so the panel never spills off the screen edge
    // when the icon sits near the right corner.
    if let Ok(Some(monitor)) = window.current_monitor() {
        let area = monitor.work_area();
        let margin = (8.0 * scale) as i32;
        let min_x = area.position.x + margin;
        let max_x = area.position.x + area.size.width as i32 - win_w - margin;
        if max_x >= min_x {
            x = x.clamp(min_x, max_x);
        }
    }

    let _ = window.set_position(PhysicalPosition::new(x, y));
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![daemon, open_main])
        .setup(|app| {
            // Right-click menu: the escape hatches that must always work.
            let open = MenuItem::with_id(app, "open", "Open Copycat", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Copycat", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;

            // One tray icon. Left-click toggles the panel; right-click drops the
            // menu. The daemon's own icon stands in until a real tray asset.
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("no default window icon to use for the tray")?;

            TrayIconBuilder::with_id("copycat")
                .icon(icon)
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
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Close the panel when it loses focus, the way a menu-bar dropdown
            // does — click anywhere else and it goes away.
            if let Some(panel) = app.get_webview_window("panel") {
                let hide_target = panel.clone();
                panel.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        let _ = hide_target.hide();
                    }
                });
            }

            // Closing the main window hides it rather than quitting: this is a
            // menu-bar app and lives in the tray.
            if let Some(main) = app.get_webview_window("main") {
                let hide_target = main.clone();
                main.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = hide_target.hide();
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
                    let payload = match call(&socket_path(), Action::Status) {
                        Ok(body) => serde_json::json!({ "connected": true, "status": body }),
                        Err(error) => serde_json::json!({ "connected": false, "error": error.message }),
                    };
                    let _ = handle.emit("daemon-state", payload);
                    std::thread::sleep(Duration::from_millis(600));
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Copycat failed to start");
}
