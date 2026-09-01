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
