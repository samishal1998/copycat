//! Starting and stopping the daemon from the CLI.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use copycat_core::{CoreError, ErrorKind};
use copycat_protocol::Action;

use crate::cli::DaemonCommand;
use crate::render;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(command: &DaemonCommand, socket: &Path, json: bool) -> Result<(), CoreError> {
    match command {
        DaemonCommand::Start { foreground, args } => start(socket, *foreground, args),
        DaemonCommand::Stop => stop(socket),
        DaemonCommand::Restart => {
            // Ignore "not running": restart should end with a daemon running,
            // whatever the starting state.
            let _ = stop(socket);
            wait_for(socket, false)?;
            start(socket, false, &[])
        }
        DaemonCommand::Status => status(socket, json),
    }
}

fn start(socket: &Path, foreground: bool, extra: &[String]) -> Result<(), CoreError> {
    if copycat_protocol::is_running(socket) {
        println!("already running at {}", socket.display());
        return Ok(());
    }

    let program = daemon_binary()?;
    let mut command = Command::new(&program);
    command.arg("--socket").arg(socket);
    command.args(extra);

    if foreground {
        let status = command.status().map_err(|e| spawn_failed(&program, e))?;
        return match status.success() {
            true => Ok(()),
            false => Err(CoreError::new(
                ErrorKind::DaemonUnavailable,
                "daemon_exited",
                format!("copycatd exited with {status}"),
            )),
        };
    }

    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().map_err(|e| spawn_failed(&program, e))?;

    wait_for(socket, true)?;
    println!("started at {}", socket.display());
    Ok(())
}

fn stop(socket: &Path) -> Result<(), CoreError> {
    copycat_protocol::call(socket, Action::DaemonStop)?;
    println!("stopping");
    Ok(())
}

fn status(socket: &Path, json: bool) -> Result<(), CoreError> {
    match copycat_protocol::call(socket, Action::Status) {
        Ok(body) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            } else {
                println!("{}", render::result(&body));
            }
            Ok(())
        }
        Err(error) if error.kind == ErrorKind::DaemonUnavailable => {
            println!("not running ({})", socket.display());
            Err(error)
        }
        Err(error) => Err(error),
    }
}

/// Poll until the socket reaches the wanted state.
///
/// Starting is asynchronous — the process has to bind before it is usable — so
/// returning immediately would make `daemon start && copycat status` a race.
fn wait_for(socket: &Path, running: bool) -> Result<(), CoreError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if copycat_protocol::is_running(socket) == running {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(CoreError::new(
        ErrorKind::DaemonUnavailable,
        if running { "daemon_did_not_start" } else { "daemon_did_not_stop" },
        format!(
            "the daemon did not {} within {}s at {}",
            if running { "start" } else { "stop" },
            STARTUP_TIMEOUT.as_secs(),
            socket.display()
        ),
    ))
}

/// Prefer the `copycatd` sitting beside this binary.
///
/// An installed pair and a `target/debug` pair should each find their own
/// partner; falling straight through to `PATH` would let a development build
/// silently drive an installed daemon.
fn daemon_binary() -> Result<std::path::PathBuf, CoreError> {
    let name = if cfg!(windows) { "copycatd.exe" } else { "copycatd" };
    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let sibling = dir.join(name);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(std::path::PathBuf::from(name))
}

fn spawn_failed(program: &Path, error: std::io::Error) -> CoreError {
    CoreError::new(
        ErrorKind::DaemonUnavailable,
        "daemon_not_found",
        format!("could not run {}: {error}", program.display()),
    )
}
