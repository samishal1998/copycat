//! The local socket, and who is allowed to talk to it.
//!
//! The daemon serves *decrypted* clipboard history here. That makes this
//! socket the trust boundary, not the database file (ADR-010): encrypting
//! payloads at rest while leaving the endpoint open would protect the disk and
//! nothing else. So the endpoint is restricted at creation and every peer is
//! checked on connect.
//!
//! Same-uid processes remain trusted. Defending a clipboard daemon against code
//! already running as its own user is not achievable and pretending otherwise
//! would be worse than saying so.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use copycat_protocol::{Request, read_frame, write_frame};
use interprocess::local_socket::{
    GenericFilePath, Listener, ListenerOptions, Stream, prelude::*,
};

use crate::server::DaemonEvent;

/// Bind the socket, replacing one left behind by a dead daemon.
pub fn bind(socket: &Path) -> Result<Listener> {
    if let Some(parent) = socket.parent() {
        crate::paths::create_private_dir(parent)?;
        crate::paths::verify_private_dir(parent)?;
    }

    match listen(socket) {
        Ok(listener) => {
            restrict(socket)?;
            Ok(listener)
        }
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // Either a daemon is already running, or the last one died without
            // cleaning up. Connecting is the only way to tell them apart.
            if Stream::connect(name_for(socket)?).is_ok() {
                anyhow::bail!("a daemon is already listening on {}", socket.display());
            }
            std::fs::remove_file(socket).ok();
            let listener = listen(socket)
                .with_context(|| format!("binding {}", socket.display()))?;
            restrict(socket)?;
            tracing::info!(socket = %socket.display(), "reclaimed a stale socket");
            Ok(listener)
        }
        Err(e) => Err(e).with_context(|| format!("binding {}", socket.display())),
    }
}

fn name_for(socket: &Path) -> Result<interprocess::local_socket::Name<'_>> {
    socket
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("{} is not a usable socket path", socket.display()))
}

fn listen(socket: &Path) -> io::Result<Listener> {
    let name = socket
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    ListenerOptions::new().name(name).create_sync()
}

/// `0600` on the socket itself. The `0700` parent directory is what actually
/// closes the window between bind and chmod; this is the second layer.
#[cfg(unix)]
fn restrict(socket: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", socket.display()))
}

#[cfg(not(unix))]
fn restrict(_socket: &Path) -> Result<()> {
    // Windows named pipes need an explicit user-only DACL and
    // PIPE_REJECT_REMOTE_CLIENTS (§23.1). `interprocess` does not expose either,
    // so this is unimplemented rather than done — `doctor` reports it, and
    // ADR-015 keeps Windows experimental until it is real.
    Ok(())
}

/// Accept connections until the listener is dropped, one thread per client.
///
/// Clients are few and short-lived — a CLI invocation, a TUI — so a thread each
/// costs less than an async runtime would.
pub fn serve(listener: Listener, tx: Sender<DaemonEvent>, socket: PathBuf) {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let tx = tx.clone();
                let socket = socket.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle(stream, &tx) {
                        tracing::debug!(socket = %socket.display(), error = %error, "client ended");
                    }
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, "accept failed");
                return;
            }
        }
    }
}

fn handle(mut stream: Stream, tx: &Sender<DaemonEvent>) -> Result<()> {
    authorize(&stream)?;

    loop {
        let Some(request): Option<Request> = read_frame(&mut stream)? else {
            return Ok(());
        };

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if tx.send(DaemonEvent::Request { request, reply: reply_tx }).is_err() {
            return Ok(()); // the daemon is shutting down
        }
        let Ok(response) = reply_rx.recv() else {
            return Ok(());
        };
        write_frame(&mut stream, &response)?;
    }
}

/// Reject any peer that is not the user the daemon runs as (§23.1).
#[cfg(unix)]
fn authorize(stream: &Stream) -> Result<()> {
    let creds = stream.peer_creds().context("reading peer credentials")?;
    let ours = unsafe { libc::geteuid() };
    match creds.euid() {
        Some(euid) if euid == ours => Ok(()),
        Some(euid) => {
            tracing::warn!(peer_uid = euid, daemon_uid = ours, "rejected a connection");
            anyhow::bail!("peer uid {euid} is not {ours}")
        }
        // No credentials means no basis to trust the peer. The socket's mode
        // and directory should already have prevented this, so refusing is the
        // only consistent answer.
        None => anyhow::bail!("the platform reported no peer uid"),
    }
}

#[cfg(not(unix))]
fn authorize(_stream: &Stream) -> Result<()> {
    Ok(())
}
