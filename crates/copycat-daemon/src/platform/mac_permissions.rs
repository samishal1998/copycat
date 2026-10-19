//! What macOS privacy controls (TCC) have actually granted this process.
//!
//! The grant belongs to the *responsible* app: the terminal when copycatd is
//! started from one, Copycat.app when the app starts it. So the same daemon
//! can work from a terminal and be refused under the app, and without asking
//! the OS directly nothing in the daemon can tell the two apart.
//!
//! The event tap is created once, and a refusal sticks until the daemon
//! restarts. So a grant made while it runs needs a restart to take effect,
//! even when these probes already report it.

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// Returns a C `Boolean` (an unsigned char), not a Rust `bool`.
    fn AXIsProcessTrusted() -> u8;
}

#[derive(Debug, Clone, Copy)]
pub struct Permissions {
    /// The Accessibility list.
    pub accessibility: bool,
    /// Posting events: paste injection, and an event tap that consumes keys.
    pub post_events: bool,
    /// Input Monitoring: listening to keys.
    pub listen_events: bool,
}

impl Permissions {
    /// Enough for the tap and for injection. The two Accessibility probes
    /// are trusted either way round, because each has been reported to answer
    /// "no" while the other answers "yes".
    pub fn keyboard_granted(&self) -> bool {
        self.accessibility || self.post_events
    }
}

pub fn current() -> Permissions {
    unsafe {
        Permissions {
            accessibility: AXIsProcessTrusted() != 0,
            post_events: CGPreflightPostEventAccess(),
            listen_events: CGPreflightListenEventAccess(),
        }
    }
}

/// Ask macOS to prompt for Accessibility. It also adds the responsible app to
/// the Accessibility list, so the user has a switch to turn on instead of a
/// "+" to hunt with.
pub fn request_keyboard_access() {
    unsafe {
        CGRequestPostEventAccess();
    }
}
