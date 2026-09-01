//! `copycat doctor`.
//!
//! The point is to make "nothing happened when I pressed the key" answerable.
//! Every check names a capability and says whether it is present, degraded, or
//! missing, and a missing one carries the reason rather than a shrug.

use copycat_protocol::{Capability, DoctorCheck, DoctorReport};

use crate::platform::DisplayServer;
use crate::server::Server;
use crate::store::KeyStorage;

pub fn report(server: &Server) -> DoctorReport {
    let display_server = server.display_server();
    let mut checks = Vec::new();
    let mut capabilities = Vec::new();

    // --- clipboard -------------------------------------------------------
    let readable = server.readable_media_types();
    checks.push(if readable.is_empty() {
        DoctorCheck::unavailable(
            "clipboard",
            format!("no usable backend ({})", server.clipboard_backend_name()),
        )
    } else {
        DoctorCheck::ok(
            "clipboard",
            format!("{} reads {}", server.clipboard_backend_name(), readable.join(", ")),
        )
    });

    let config = server.config();
    if config.history.capture_html && !readable.iter().any(|t| t == copycat_core::TEXT_HTML) {
        // A config asking for something the backend cannot do should be visible
        // here rather than quietly ignored.
        checks.push(DoctorCheck::degraded(
            "html-capture",
            "history.capture_html is on, but this backend can only read plain text",
        ));
    }
    if config.history.capture_images && !readable.iter().any(|t| t.starts_with("image/")) {
        checks.push(DoctorCheck::degraded(
            "image-capture",
            "history.capture_images is on, but this backend can only read plain text",
        ));
    }

    // --- input -----------------------------------------------------------
    checks.push(match server.injector_name() {
        "unavailable" => DoctorCheck::unavailable(
            "paste-injection",
            "the paste chord cannot be delivered; pastes will land on the clipboard only",
        ),
        name => DoctorCheck::ok("paste-injection", name.to_string()),
    });

    let hotkeys = server.hotkey_registry();
    checks.push(if hotkeys.available() {
        DoctorCheck::ok(
            "global-hotkeys",
            format!("{} registered", hotkeys.registered_count()),
        )
    } else {
        DoctorCheck::unavailable("global-hotkeys", "no global shortcut backend on this platform")
    });
    for rejected in hotkeys.rejected() {
        checks.push(DoctorCheck::degraded(
            "binding",
            format!("{}: {}", rejected.trigger, rejected.reason),
        ));
    }

    checks.push(if display_server.supports_leader_sequences() {
        DoctorCheck::ok("leader-sequences", "the next key press can be observed")
    } else {
        DoctorCheck::unavailable(
            "leader-sequences",
            format!(
                "{} does not let a client observe the next key press; use direct hotkeys",
                display_server.as_str()
            ),
        )
    });

    // --- storage and keys ------------------------------------------------
    checks.push(match server.key_storage() {
        KeyStorage::Keyring => DoctorCheck::ok("key-storage", "the OS keyring holds the key"),
        KeyStorage::KeyFile => DoctorCheck::degraded(
            "key-storage",
            "no OS keyring; the key is a 0600 file, which is weaker than the keyring",
        ),
        KeyStorage::MemoryOnly => DoctorCheck::degraded(
            "key-storage",
            "no key available; nothing is written to disk this session",
        ),
    });

    checks.push(match server.store() {
        Some(store) => match store.count() {
            Ok(count) => DoctorCheck::ok("history-store", format!("{count} clips persisted")),
            Err(error) => DoctorCheck::unavailable("history-store", format!("{error:#}")),
        },
        None => DoctorCheck::degraded("history-store", "persistence is off"),
    });

    let paths = server.paths();
    checks.push(match crate::paths::verify_private_dir(&paths.data_dir) {
        Ok(()) => DoctorCheck::ok("data-directory", paths.data_dir.display().to_string()),
        Err(error) => DoctorCheck::unavailable("data-directory", format!("{error:#}")),
    });

    // --- ipc -------------------------------------------------------------
    checks.push(if cfg!(unix) {
        DoctorCheck::ok(
            "ipc-access-control",
            "peer uid is checked on every connection; socket is 0600 in a 0700 directory",
        )
    } else {
        // Being explicit is the whole point: this is a §23.1 requirement that
        // is not yet implemented, and a silent pass would be a lie.
        DoctorCheck::degraded(
            "ipc-access-control",
            "the named pipe does not yet carry a user-only DACL (section 23.1 is unimplemented on Windows)",
        )
    });

    // --- unimplemented settings the user may have switched on ------------
    if config.privacy.pause_on_lock_screen {
        checks.push(DoctorCheck::degraded(
            "pause-on-lock",
            "privacy.pause_on_lock_screen is on but no platform implementation exists yet (R20)",
        ));
    }

    capabilities.extend(server.platform_notes().iter().cloned());
    capabilities.push(Capability {
        name: "clipboard-formats".into(),
        available: !readable.is_empty(),
        detail: if readable.is_empty() { "none".into() } else { readable.join(", ") },
    });

    DoctorReport {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        display_server: display_server.as_str().to_string(),
        platform_support: platform_support(display_server).to_string(),
        checks,
        capabilities,
    }
}

/// Which platforms have actually been exercised (§20.2, ADR-015).
///
/// This says "verified" only where it is true. A backend that compiles is not a
/// backend that works, and the difference is the user's afternoon.
fn platform_support(display_server: DisplayServer) -> &'static str {
    match display_server {
        DisplayServer::X11 => "reference platform",
        DisplayServer::Headless => "no display: CLI and IPC work, capture and injection do not",
        DisplayServer::Wayland => {
            "experimental: clipboard via XWayland, no leader sequences"
        }
        DisplayServer::MacOs | DisplayServer::Windows => {
            "experimental: written but not yet verified against the adapter contract suite"
        }
    }
}
