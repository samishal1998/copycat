//! The Copycat daemon.
//!
//! Owns the clipboard, the history, and the session state; every interface is a
//! client of this process over a local socket (ADR-003).

mod bindings;
mod config;
mod config_edit;
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

    /// Also write the log to this file. `-` disables the file entirely.
    /// Defaults to `copycat.log` in the data directory, so a detached daemon
    /// still leaves a journal `copycat logs` can read.
    #[arg(long, value_name = "PATH")]
    log_file: Option<PathBuf>,
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

    let paths = Paths::resolve(cli.config.clone(), cli.data_dir.clone(), cli.socket.clone())?;
    paths.prepare()?;
    init_logging(&cli, &paths);

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
    let (events_tx, events_rx) = std::sync::mpsc::channel();

    // What the platform thread calls when it catches the paste chord. It
    // blocks until the daemon has written the next item, because the keystroke
    // must not reach the application before the clipboard holds it - and gives
    // up after a bound so a stuck daemon cannot freeze the keyboard.
    let on_paste_chord: platform::intercept::Handler = {
        let events = events_tx.clone();
        std::sync::Arc::new(move || {
            let (reply, done) = std::sync::mpsc::channel();
            if events.send(server::DaemonEvent::PasteChord { reply }).is_ok() {
                let _ = done.recv_timeout(platform::intercept::HANDLER_TIMEOUT);
            }
        })
    };
    // Hotkeys and leader keys from a platform that delivers them itself (the
    // macOS event tap). Fire-and-forget: nothing is waiting on the reply.
    let on_hotkey: std::sync::Arc<dyn Fn(u32) + Send + Sync> = {
        let events = events_tx.clone();
        std::sync::Arc::new(move |id| {
            let _ = events.send(server::DaemonEvent::Hotkey(id));
        })
    };
    let on_leader_key: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync> = {
        let events = events_tx.clone();
        std::sync::Arc::new(move |key| {
            let _ = events.send(server::DaemonEvent::LeaderKey(key));
        })
    };
    let platform = platform::select(
        choice,
        platform::PlatformEvents { on_hotkey, on_leader_key, on_paste_chord },
    );

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

fn init_logging(cli: &Cli, paths: &Paths) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_env("COPYCAT_LOG")
        .unwrap_or_else(|_| EnvFilter::new(cli.log.clone()));

    // Where the file log goes: the flag, or the default beside the data. `-`
    // turns it off.
    let log_file = match &cli.log_file {
        Some(path) if path.as_os_str() == "-" => None,
        Some(path) => Some(path.clone()),
        None => Some(paths.data_dir.join("copycat.log")),
    };

    // Payload bytes never reach a log at any level (§23.3): only ids, hash
    // prefixes, sizes, and error kinds.
    let json = matches!(cli.log_format, LogFormat::Json);

    // The console layer. `.boxed()` erases the format difference so both
    // branches build the same registry type.
    let console = if json {
        fmt::layer().json().with_writer(std::io::stderr).boxed()
    } else {
        fmt::layer().with_writer(std::io::stderr).boxed()
    };

    // The file layer, when a file is wanted and openable. A fresh dup of the
    // fd per event; O_APPEND keeps writes from interleaving. Cheap enough for
    // a daemon's log volume, and it avoids a new dependency just to log to a
    // file.
    let file = log_file.as_ref().and_then(|path| {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(handle) => {
                let make = move || handle.try_clone().expect("clone log file handle");
                Some(if json {
                    fmt::layer().json().with_writer(make).boxed()
                } else {
                    fmt::layer().with_ansi(false).with_writer(make).boxed()
                })
            }
            Err(error) => {
                eprintln!("copycatd: cannot open log file {}: {error}", path.display());
                None
            }
        }
    });

    tracing_subscriber::registry().with(filter).with(console).with(file).init();

    if let Some(path) = &log_file {
        tracing::info!(log = %path.display(), "logging to file");
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
