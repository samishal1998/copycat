//! Adapters between the state machine and the operating system.
//!
//! Every trait here exists so `copycat-core` can be tested without one. The
//! important rule (ADR-008) is that a capability the platform does not have is
//! *named*, not silently absent: a leader sequence that cannot work on this
//! compositor should say so in `doctor`, not fail quietly when the key is
//! pressed.

pub mod clipboard;
pub mod file;
pub mod hotkey;
pub mod inject;

use copycat_core::{ClipPayload, CoreError, ErrorKind};
use copycat_protocol::Capability;

pub type Result<T> = std::result::Result<T, CoreError>;

/// Read and write the system clipboard.
pub trait ClipboardBackend: Send {
    fn read(&mut self) -> Result<ClipPayload>;
    fn write(&mut self, payload: &ClipPayload) -> Result<()>;

    /// A value that changes on *every* copy, including one that copies the
    /// same text again.
    ///
    /// This is not an optimization. Without it the watcher can only compare
    /// content, so copying the same value twice is invisible — which would
    /// mean consecutive duplicates never reach the raw log, and
    /// `--duplicates preserve` could never preserve anything (ADR-002).
    ///
    /// macOS exposes exactly this as `NSPasteboard.changeCount`, Windows
    /// through its clipboard listener, and X11 through XFixes selection
    /// notifications. `None` means this backend cannot tell a repeat copy from
    /// no copy, and `doctor` says so rather than letting the duplicate policy
    /// quietly do nothing.
    fn change_token(&mut self) -> Option<u64> {
        None
    }
    /// Media types this backend can actually *read*. Reported by `doctor`, so
    /// a config asking for HTML capture on a text-only backend is visible
    /// rather than mysterious.
    fn readable_media_types(&self) -> Vec<String>;
    fn name(&self) -> String;
}

/// Send the platform's paste chord to the focused application.
pub trait PasteInjector: Send {
    fn inject(&mut self) -> Result<()>;
    fn name(&self) -> String;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    X11,
    Wayland,
    Windows,
    MacOs,
    /// A Unix session with no display: a TTY, a container, CI.
    Headless,
}

impl DisplayServer {
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayServer::X11 => "x11",
            DisplayServer::Wayland => "wayland",
            DisplayServer::Windows => "windows",
            DisplayServer::MacOs => "macos",
            DisplayServer::Headless => "headless",
        }
    }

    /// Whether a tmux-style leader sequence can work here.
    ///
    /// A leader needs to observe the *next* key after the trigger, which is a
    /// different capability from registering one global shortcut. Wayland
    /// offers the latter through the portal and not the former, which is why
    /// this is a capability question rather than a bug (ADR-008).
    pub fn supports_leader_sequences(self) -> bool {
        matches!(self, DisplayServer::X11)
    }
}

pub fn detect_display_server() -> DisplayServer {
    if cfg!(target_os = "windows") {
        return DisplayServer::Windows;
    }
    if cfg!(target_os = "macos") {
        return DisplayServer::MacOs;
    }
    let has = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if has("WAYLAND_DISPLAY") {
        DisplayServer::Wayland
    } else if has("DISPLAY") {
        DisplayServer::X11
    } else {
        DisplayServer::Headless
    }
}

/// Which backends to build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// Whatever the platform offers.
    #[default]
    Auto,
    /// A clipboard backed by a file, for machines with no display.
    File(std::path::PathBuf),
}

pub struct Platform {
    pub clipboard: Box<dyn ClipboardBackend>,
    pub injector: Box<dyn PasteInjector>,
    pub display_server: DisplayServer,
    /// Problems found while selecting backends, for `doctor` to report.
    pub notes: Vec<Capability>,
}

pub fn select(choice: BackendChoice) -> Platform {
    let display_server = detect_display_server();
    let mut notes = Vec::new();

    if let BackendChoice::File(path) = choice {
        let detail = format!("file-backed clipboard at {}", path.display());
        return Platform {
            clipboard: Box::new(file::FileClipboard::new(path)),
            injector: Box::new(file::NoopInjector),
            display_server,
            notes: vec![Capability { name: "clipboard".into(), available: true, detail }],
        };
    }

    let clipboard: Box<dyn ClipboardBackend> = match clipboard::SystemClipboard::new() {
        Ok(backend) => Box::new(backend),
        Err(error) => {
            notes.push(Capability {
                name: "clipboard".into(),
                available: false,
                detail: error.message.clone(),
            });
            Box::new(UnavailableClipboard { reason: error.message })
        }
    };

    let injector: Box<dyn PasteInjector> = match inject::system_injector() {
        Ok(backend) => backend,
        Err(error) => {
            notes.push(Capability {
                name: "paste-injection".into(),
                available: false,
                detail: error.message.clone(),
            });
            Box::new(UnavailableInjector { reason: error.message })
        }
    };

    Platform { clipboard, injector, display_server, notes }
}

/// Stands in for a backend that could not be created, so the daemon still runs
/// and `doctor` can explain why nothing works.
struct UnavailableClipboard {
    reason: String,
}

impl ClipboardBackend for UnavailableClipboard {
    fn read(&mut self) -> Result<ClipPayload> {
        Err(CoreError::new(ErrorKind::ClipboardUnavailable, "clipboard_unavailable", self.reason.clone()))
    }

    fn write(&mut self, _payload: &ClipPayload) -> Result<()> {
        Err(CoreError::new(ErrorKind::ClipboardUnavailable, "clipboard_unavailable", self.reason.clone()))
    }

    fn readable_media_types(&self) -> Vec<String> {
        Vec::new()
    }

    fn name(&self) -> String {
        "unavailable".into()
    }
}

struct UnavailableInjector {
    reason: String,
}

impl PasteInjector for UnavailableInjector {
    fn inject(&mut self) -> Result<()> {
        Err(CoreError::new(ErrorKind::PlatformUnavailable, "injection_unavailable", self.reason.clone()))
    }

    fn name(&self) -> String {
        "unavailable".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_free_unix_session_is_reported_as_headless() {
        // Guards the honest-capability rule: no display must not be mistaken
        // for X11 and then fail at the first paste.
        if cfg!(unix) && !cfg!(target_os = "macos") {
            let server = detect_display_server();
            let expected = match (
                std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
                std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()),
            ) {
                (true, _) => DisplayServer::Wayland,
                (false, true) => DisplayServer::X11,
                _ => DisplayServer::Headless,
            };
            assert_eq!(server, expected);
        }
    }

    #[test]
    fn leader_sequences_are_only_claimed_where_they_can_work() {
        assert!(DisplayServer::X11.supports_leader_sequences());
        assert!(!DisplayServer::Wayland.supports_leader_sequences());
        assert!(!DisplayServer::Headless.supports_leader_sequences());
    }

    #[test]
    fn an_unavailable_backend_errors_with_a_reason_rather_than_pretending() {
        let mut clipboard = UnavailableClipboard { reason: "no DISPLAY".into() };
        let error = clipboard.read().unwrap_err();
        assert_eq!(error.exit_code(), 4);
        assert!(error.message.contains("no DISPLAY"));
    }
}
