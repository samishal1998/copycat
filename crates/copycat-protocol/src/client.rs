//! Connecting to the daemon.
//!
//! Lives here rather than in each client so the CLI, the TUI, and anything
//! else all speak to the socket the same way — including the "is the daemon
//! even running" answer, which is the most common failure and deserves one
//! message rather than three.

use std::path::{Path, PathBuf};

use copycat_core::{CoreError, ErrorKind};
use interprocess::local_socket::{GenericFilePath, Stream, prelude::*};

use crate::{Action, Outcome, Request, ResultBody, read_frame, write_frame};

/// Send one request and wait for its response.
pub fn request(socket: &Path, request: &Request) -> Result<ResultBody, CoreError> {
    let name = socket.to_fs_name::<GenericFilePath>().map_err(|e| {
        CoreError::invalid("bad_socket_path", format!("{}: {e}", socket.display()))
    })?;

    let mut stream = Stream::connect(name).map_err(|e| {
        CoreError::new(
            ErrorKind::DaemonUnavailable,
            "daemon_unavailable",
            format!("cannot reach the daemon at {}: {e}", socket.display()),
        )
    })?;

    write_frame(&mut stream, request).map_err(transport)?;
    let response: crate::Response = read_frame(&mut stream)
        .map_err(transport)?
        .ok_or_else(|| {
            CoreError::new(
                ErrorKind::DaemonUnavailable,
                "daemon_closed",
                "the daemon closed the connection without replying",
            )
        })?;

    match response.outcome {
        Outcome::Ok { result } => Ok(result),
        Outcome::Error { error } => Err(error),
    }
}

/// Send one action, generating the request id.
pub fn call(socket: &Path, action: Action) -> Result<ResultBody, CoreError> {
    request(socket, &Request::new(next_id(), action))
}

fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!("req-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

fn transport(error: crate::FrameError) -> CoreError {
    CoreError::new(ErrorKind::DaemonUnavailable, "transport_error", format!("{error}"))
}

/// Whether a daemon is listening.
pub fn is_running(socket: &Path) -> bool {
    socket
        .to_fs_name::<GenericFilePath>()
        .ok()
        .and_then(|name| Stream::connect(name).ok())
        .is_some()
}

/// The directory name Copycat uses under the platform's config, data, and
/// runtime roots.
pub const APP_DIR: &str = "copycat";

/// Where the daemon listens by default.
///
/// This lives in the protocol crate because a client and the daemon disagreeing
/// about the socket path is indistinguishable, from the user's side, from the
/// daemon not running.
pub fn default_socket_path() -> Option<PathBuf> {
    if cfg!(windows) {
        // Interpreted as a named-pipe name rather than a filesystem path.
        return Some(PathBuf::from(format!(
            "copycat-{}",
            std::env::var("USERNAME").unwrap_or_else(|_| "user".into())
        )));
    }
    // A runtime directory is the right home for a socket: it is on tmpfs and is
    // cleared on logout, so a stale socket cannot outlive the session.
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => {
            Some(PathBuf::from(dir).join(APP_DIR).join("daemon.sock"))
        }
        _ => dirs::data_dir().map(|dir| dir.join(APP_DIR).join("daemon.sock")),
    }
}

/// Canonical spelling for a chord's modifier names.
///
/// Every platform has its own word for the same physical key, and people write
/// whichever one their OS taught them: `cmd` and `command` on macOS, `win` and
/// `windows` on Windows, `super` and `meta` on Linux. They all mean the key
/// next to the space bar that is not Alt.
///
/// This exists because the shortcut backend only knows some of those names and
/// rejects the rest as if they were *key* names — "Couldn't recognize `meta` as
/// a valid key" — which sends people looking at the wrong half of the chord.
/// Normalizing happens on the way to the backend, so the config keeps whatever
/// the user wrote.
///
/// Note what this does *not* do: it does not translate `ctrl` to `cmd` on
/// macOS. `ctrl+c` means Control there, as it should. Use `cmdorctrl` for a
/// chord that should follow the platform convention.
pub fn normalize_trigger(trigger: &str) -> String {
    trigger
        .split('+')
        .map(|token| {
            let token = token.trim();
            match token.to_ascii_lowercase().as_str() {
                // Command on macOS, Windows key on Windows, Super on Linux.
                "meta" | "win" | "windows" | "cmd" | "command" | "super" => "super".to_string(),
                // Option on macOS.
                "opt" | "option" | "alt" => "alt".to_string(),
                "ctrl" | "control" => "ctrl".to_string(),
                "shift" => "shift".to_string(),
                // Command on macOS, Control elsewhere.
                "cmdorctrl" | "cmdorcontrol" | "commandorctrl" | "commandorcontrol" => {
                    "cmdorctrl".to_string()
                }
                _ => token.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// The modifier names a chord may use, for error messages.
pub const MODIFIER_NAMES: &str =
    "ctrl/control, alt/option, cmd/command/super/meta/win, shift, cmdorctrl";

#[cfg(test)]
mod trigger_tests {
    use super::normalize_trigger;

    #[test]
    fn every_name_for_the_command_key_lands_on_the_same_modifier() {
        for spelling in ["cmd", "command", "super", "meta", "win", "windows", "Meta", "WIN"] {
            assert_eq!(
                normalize_trigger(&format!("{spelling}+v")),
                "super+v",
                "{spelling} should be accepted"
            );
        }
    }

    #[test]
    fn option_and_alt_are_the_same_key() {
        assert_eq!(normalize_trigger("option+v"), "alt+v");
        assert_eq!(normalize_trigger("opt+shift+v"), "alt+shift+v");
    }

    #[test]
    fn control_is_never_silently_turned_into_command() {
        // ctrl+c means Control on macOS too. Quietly rewriting it would break
        // every chord someone deliberately wanted on Control.
        assert_eq!(normalize_trigger("ctrl+c"), "ctrl+c");
        assert_eq!(normalize_trigger("cmdorctrl+c"), "cmdorctrl+c");
    }

    #[test]
    fn the_key_itself_is_left_alone() {
        assert_eq!(normalize_trigger("ctrl+alt+space"), "ctrl+alt+space");
        assert_eq!(normalize_trigger("F5"), "F5");
        assert_eq!(normalize_trigger(" ctrl + alt + v "), "ctrl+alt+v");
    }
}
