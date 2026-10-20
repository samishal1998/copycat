//! Finding and starting `copycatd`, for the desktop apps.
//!
//! Every GUI is a client of the daemon, and every one of them has to answer
//! the same questions first: is a daemon running, is it new enough for this
//! build, and if not, which `copycatd` should be started. The answers live
//! here once, so the Tauri app and the GPUI app cannot drift apart on them.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use copycat_protocol::{Action, ResultBody, call, default_socket_path};

/// Where the daemon listens. Shared with the CLI through the protocol crate, so
/// the GUI and CLI can never disagree about the path.
pub fn socket_path() -> PathBuf {
    default_socket_path().unwrap_or_else(|| PathBuf::from("copycat.sock"))
}

/// Restart the daemon so a freshly granted permission takes effect — the
/// event tap is created once, and a refusal sticks until a fresh process.
/// Blocks for up to a few seconds; call it off the UI thread.
pub fn restart_daemon() {
    let socket = socket_path();
    let _ = call(&socket, Action::DaemonStop);
    for _ in 0..30 {
        if !copycat_protocol::is_running(&socket) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ensure_daemon();
}

/// Start the daemon if it is not already running — and take over an incompatible
/// one when we ship our own.
///
/// Opening the app and being told "daemon offline" is a poor first run, so the
/// GUI brings the daemon up itself. It does not stop a *compatible* daemon on
/// quit — that is a background service the CLI and other clients share, and the
/// GUI is one client, not its owner.
///
/// The one exception is a daemon too old for this build: an older `copycatd`
/// (installed by a package manager, say) can be missing actions this GUI relies
/// on. When we bundle our own daemon we replace the stale one with it; when we
/// do not (a `tauri dev` build with no sidecar) we leave the user's only daemon
/// alone rather than kill it for a replacement we cannot start.
pub fn ensure_daemon() {
    let socket = socket_path();
    if copycat_protocol::is_running(&socket) {
        let compatible = running_daemon_version(&socket)
            .map(|v| version_at_least(&v, copycat_protocol::VERSION))
            // No readable version means an ancient or broken daemon; treat it
            // as incompatible so a bundled build can take over.
            .unwrap_or(false);
        if compatible {
            return;
        }
        if find_bundled().is_none() && compatible_installed().is_none() {
            return; // no replacement we can start — don't strand the user
        }
        let _ = call(&socket, Action::DaemonStop);
        for _ in 0..30 {
            if !copycat_protocol::is_running(&socket) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
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

/// The version the running daemon reports, or `None` if it cannot be asked.
fn running_daemon_version(socket: &Path) -> Option<String> {
    match call(socket, Action::Status) {
        Ok(ResultBody::Status(report)) => Some(report.daemon_version),
        _ => None,
    }
}

/// Whether `have` (a daemon's version) is new enough for a client built against
/// `want`. A newer daemon than client is the safe direction, so it is accepted.
pub fn version_at_least(have: &str, want: &str) -> bool {
    semver_cmp(have, want) != std::cmp::Ordering::Less
}

/// Semver precedence: a pre-release ranks *below* its release
/// (`0.5.0-rc.5 < 0.5.0 < 0.6.0-rc.1`), and pre-release identifiers compare
/// numerically when both are numbers (`rc.10 > rc.9`), lexically otherwise.
/// Ignoring the pre-release is what once let a stale `0.5.0-rc.5` daemon pass
/// for `0.5.0`.
fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn split(v: &str) -> ((u64, u64, u64), Option<&str>) {
        let v = v.split('+').next().unwrap_or(v); // build metadata never counts
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        let mut n = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        ((n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0)), pre)
    }
    let ((core_a, pre_a), (core_b, pre_b)) = (split(a), split(b));
    core_a.cmp(&core_b).then_with(|| match (pre_a, pre_b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (mut xs, mut ys) = (x.split('.'), y.split('.'));
            loop {
                let order = match (xs.next(), ys.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(p), Some(q)) => match (p.parse::<u64>(), q.parse::<u64>()) {
                        (Ok(m), Ok(n)) => m.cmp(&n),
                        (Ok(_), Err(_)) => Ordering::Less, // numeric sorts below alphanumeric
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => p.cmp(q),
                    },
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    })
}

/// The daemon bundled beside the GUI, if this build ships one (the sidecar).
/// An empty file is a build's stand-in, not a daemon.
fn find_bundled() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.parent()?.join("copycatd");
    beside.metadata().is_ok_and(|m| m.is_file() && m.len() > 0).then_some(beside)
}

/// A `copycatd` installed system-wide (the CLI installer or a package manager),
/// as opposed to the one we bundle. Drives the "install the CLI" nudge and
/// [`find_copycatd`]'s choice; `None` when nothing is on disk.
pub fn find_system_copycatd() -> Option<PathBuf> {
    let name = "copycatd";
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/bin").join(name)); // installer default
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(name)); // Homebrew (Apple silicon)
    candidates.push(PathBuf::from("/usr/local/bin").join(name)); // Homebrew (Intel) / manual
    candidates.into_iter().find(|c| c.is_file())
}

/// The `copycatd` to start: the installed one when it is new enough, else the
/// bundled one.
///
/// Installed first because it is the binary the CLI starts, so it is the one
/// the keychain item trusts. Any other binary gets a keychain prompt, and a
/// refusal there leaves that daemon unable to read the history. A GUI launched
/// from Finder has a minimal `PATH`, hence the explicit locations.
fn find_copycatd() -> Option<PathBuf> {
    // Older or unreadable: fall back to our own, then to whatever is there.
    compatible_installed()
        .or_else(find_bundled)
        .or_else(find_system_copycatd)
        .or_else(|| Some(PathBuf::from("copycatd")))
}

/// The installed `copycatd`, if it is new enough for this build.
fn compatible_installed() -> Option<PathBuf> {
    find_system_copycatd().filter(|path| {
        installed_version(path).is_some_and(|v| version_at_least(&v, copycat_protocol::VERSION))
    })
}

/// What `copycatd --version` reports (`copycatd 0.6.0`), or `None`.
fn installed_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary).arg("--version").stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    text.split_whitespace().nth(1).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_is_compatible_when_at_least_as_new_as_this_build() {
        assert!(version_at_least("0.5.0", "0.5.0")); // exact match
        assert!(version_at_least("0.6.0", "0.5.0")); // newer daemon: safe direction
        assert!(version_at_least("1.0.0", "0.9.9"));
        assert!(version_at_least("0.5.1-rc.1", "0.5.0")); // pre-release of a newer patch
        assert!(!version_at_least("0.4.9", "0.5.0")); // older daemon: take over
        assert!(!version_at_least("0.4.0-rc.1", "0.5.0"));
    }

    #[test]
    fn a_pre_release_ranks_below_its_release() {
        // The bug this guards: an installed 0.5.0-rc.5 read as equal to 0.5.0.
        assert!(!version_at_least("0.5.0-rc.5", "0.5.0"));
        assert!(version_at_least("0.5.0", "0.5.0-rc.5"));
        // The released v0.5.0 daemon predates debug.events; this build replaces it.
        assert!(!version_at_least("0.5.0", "0.6.0-rc.1"));
        assert!(version_at_least("0.6.0-rc.1", "0.6.0-rc.1"));
        assert!(version_at_least("0.6.0-rc.10", "0.6.0-rc.9")); // numeric, not lexical
        assert!(!version_at_least("0.6.0-rc.9", "0.6.0-rc.10"));
        assert!(version_at_least("0.6.0-rc.1", "0.6.0-beta.3")); // alphanumeric: lexical
        assert!(version_at_least("0.6.0", "0.6.0-rc.1+build.7")); // metadata ignored
    }
}
