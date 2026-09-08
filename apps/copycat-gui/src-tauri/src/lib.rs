//! The Copycat desktop shell.
//!
//! This is deliberately thin: the daemon owns every bit of clipboard state and
//! speaks it over a local socket, so the GUI is one more client of that socket
//! (ADR-003). The frontend renders; this Rust layer does three things — resolve
//! the socket, forward a request to the daemon, and poll the daemon's state so
//! the panel stays live. No clipboard logic lives here.

use std::path::PathBuf;
use std::time::Duration;

use copycat_protocol::{Action, Request, call, default_socket_path, request};
use tauri::{
    Emitter, Manager,
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

/// Show the panel if hidden, hide it if shown — the menu-bar toggle.
fn toggle_panel(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("panel") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![daemon])
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
                    "open" => {
                        if let Some(window) = app.get_webview_window("panel") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_panel(tray.app_handle());
                    }
                })
                .build(app)?;

            // Poll the daemon and push its state to the panel. Polling, not a
            // subscription: it is what the TUI does, and a real event stream on
            // the protocol is a later optimization, not a v1 blocker.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
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
