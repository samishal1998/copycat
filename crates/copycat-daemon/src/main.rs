//! The Copycat daemon.
//!
//! Owns the clipboard, the history, and the session state; every interface is a
//! client of this process over a local socket (ADR-003).

mod bindings;
mod config;
mod doctor;
mod ipc;
mod paths;
mod platform;
mod server;
mod store;

use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use crate::config::Config;
use crate::paths::Paths;
use crate::platform::BackendChoice;
use crate::server::{DaemonEvent, Server};

#[derive(Parser, Debug)]
#[command(name = "copycatd", version, about = "The Copycat clipboard daemon")]
struct Cli {
    /// Configuration file. Defaults to the platform config directory.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Data directory for the history database and key file.
    #[arg(long, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Socket path, or named-pipe name on Windows.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Clipboard backend. `file` reads and writes a plain file instead of the
    /// system clipboard, which is how the daemon can be exercised on a machine
    /// with no display.
    #[arg(long, value_enum, default_value_t = Backend::Auto)]
    clipboard: Backend,

    /// Path for `--clipboard file`. Defaults to `clipboard.txt` in the data directory.
    #[arg(long, value_name = "PATH")]
    clipboard_file: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,

    /// Log filter, e.g. `debug` or `copycatd=debug`.
    #[arg(long, default_value = "info")]
    log: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Backend {
    Auto,
    File,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli);

    let paths = Paths::resolve(cli.config.clone(), cli.data_dir.clone(), cli.socket.clone())?;
    paths.prepare()?;

    let config = Config::load(&paths.config_file)
        .with_context(|| format!("loading {}", paths.config_file.display()))?;

    let choice = match cli.clipboard {
        Backend::Auto => BackendChoice::Auto,
        Backend::File => BackendChoice::File(
            cli.clipboard_file
                .clone()
                .unwrap_or_else(|| paths.data_dir.join("clipboard.txt")),
        ),
    };
    let platform = platform::select(choice);

    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let server = Server::new(config.clone(), paths.clone(), platform, events_tx.clone());

    let listener = ipc::bind(&paths.socket)?;
    {
        let tx = events_tx.clone();
        let socket = paths.socket.clone();
        std::thread::spawn(move || ipc::serve(listener, tx, socket));
    }

    server::spawn_watcher(
        server.shared_clipboard(),
        Duration::from_millis(config.platform.watch_interval_ms),
        server.restored_hash(),
        events_tx.clone(),
    );
    server::spawn_hotkey_listener(events_tx.clone());
    server::spawn_ticker(events_tx.clone(), Duration::from_secs(600));
    install_signal_handlers(&events_tx)?;

    let result = server.run(events_rx);

    // The socket is a filesystem object on Unix; leaving it behind would make
    // the next start take the stale-socket path for no reason.
    #[cfg(unix)]
    let _ = std::fs::remove_file(&paths.socket);

    result
}

fn init_logging(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("COPYCAT_LOG")
        .unwrap_or_else(|_| EnvFilter::new(cli.log.clone()));

    // Payload bytes never reach a log at any level (§23.3). What is logged is
    // ids, hash prefixes, sizes, and error kinds.
    match cli.log_format {
        LogFormat::Json => {
            tracing_subscriber::fmt().json().with_env_filter(filter).init();
        }
        LogFormat::Text => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

/// SIGTERM and SIGINT shut down; SIGHUP reloads the config (§14).
#[cfg(unix)]
fn install_signal_handlers(events: &std::sync::mpsc::Sender<DaemonEvent>) -> Result<()> {
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    static RELOAD: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(signal: libc::c_int) {
        // Async-signal-safe: set a flag and return. Everything else happens on
        // the polling thread below.
        match signal {
            libc::SIGHUP => RELOAD.store(true, Ordering::SeqCst),
            _ => SHUTDOWN.store(true, Ordering::SeqCst),
        }
    }

    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            if libc::signal(signal, handler as libc::sighandler_t) == libc::SIG_ERR {
                anyhow::bail!("could not install a handler for signal {signal}");
            }
        }
    }

    let events = events.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if SHUTDOWN.swap(false, Ordering::SeqCst) {
                let _ = events.send(DaemonEvent::Shutdown);
                return;
            }
            if RELOAD.swap(false, Ordering::SeqCst) {
                let request = copycat_protocol::Request::new(
                    "sighup",
                    copycat_protocol::Action::BindReload,
                );
                let (tx, _rx) = std::sync::mpsc::channel();
                if events.send(DaemonEvent::Request { request, reply: tx }).is_err() {
                    return;
                }
            }
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handlers(_events: &std::sync::mpsc::Sender<DaemonEvent>) -> Result<()> {
    Ok(())
}
