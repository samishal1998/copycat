//! Global shortcuts and tmux-style leader sequences.
//!
//! These are two different capabilities, not one feature at two sizes (§3.6,
//! ADR-008):
//!
//! * a **direct hotkey** asks the platform to deliver one chord. Every desktop
//!   platform supports this, Wayland included through the portal.
//! * a **leader sequence** asks to observe whatever key is pressed *next*.
//!   That is keyboard interception, and it is available on X11, on macOS with
//!   Accessibility permission, and on Windows with a low-level hook — but not
//!   on Wayland, whose whole design is that clients cannot watch the keyboard.
//!
//! The backend behind a [`HotkeyRegistry`] differs by platform. Linux and
//! Windows use `global-hotkey`; macOS uses the daemon's own event tap, because
//! `global-hotkey` there goes through Carbon, which a daemon without an
//! application run loop cannot use at all.

use std::str::FromStr;
use std::time::Duration;

use copycat_core::{CoreError, ErrorKind};
use copycat_protocol::RejectedBinding;
use global_hotkey::{GlobalHotKeyManager, hotkey::HotKey};

use super::DisplayServer;

/// What registers chords with the platform.
pub trait HotkeyBackend: Send {
    /// Register a chord. Returns the id the platform will report it under,
    /// which need not be the one proposed.
    fn register(&mut self, trigger: &str, proposed_id: u32) -> Result<u32, String>;

    /// Register the leader chord. By default a leader is an ordinary hotkey
    /// and the server observes the following key itself; a backend that can
    /// read the sequence in-line overrides this.
    fn register_leader(&mut self, trigger: &str, proposed_id: u32, _timeout: Duration) -> Result<u32, String> {
        self.register(trigger, proposed_id)
    }

    fn unregister_all(&mut self);
    fn unavailable_reason(&self) -> Option<String>;
    fn name(&self) -> String;

    /// Whether the backend reads the leader's sequence key itself and reports
    /// it directly, so the server must not open a second observation.
    fn observes_leader_itself(&self) -> bool {
        false
    }
}

/// The right backend for a platform, or a named reason there is none.
///
/// Not used on macOS, where the platform builds the event tap and hands out
/// its hotkey face directly.
pub fn backend_for(display_server: DisplayServer) -> Box<dyn HotkeyBackend> {
    if let Some(reason) = backend_unusable(display_server) {
        return Box::new(NoBackend { reason });
    }
    match GlobalHotkeyBackend::new() {
        Ok(backend) => Box::new(backend),
        Err(error) => Box::new(NoBackend { reason: explain_failure(display_server, &error) }),
    }
}

pub struct NoBackend {
    pub reason: String,
}

impl HotkeyBackend for NoBackend {
    fn register(&mut self, _trigger: &str, _proposed_id: u32) -> Result<u32, String> {
        Err(self.reason.clone())
    }

    fn unregister_all(&mut self) {}

    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }

    fn name(&self) -> String {
        "unavailable".into()
    }
}

/// `global-hotkey`, for X11 and Windows.
pub struct GlobalHotkeyBackend {
    manager: GlobalHotKeyManager,
    registered: Vec<HotKey>,
}

impl GlobalHotkeyBackend {
    pub fn new() -> Result<Self, global_hotkey::Error> {
        Ok(GlobalHotkeyBackend { manager: GlobalHotKeyManager::new()?, registered: Vec::new() })
    }
}

impl HotkeyBackend for GlobalHotkeyBackend {
    fn register(&mut self, trigger: &str, _proposed_id: u32) -> Result<u32, String> {
        let hotkey = HotKey::from_str(&copycat_protocol::normalize_trigger(trigger))
            .map_err(|e| format!("unparseable: {e}. Modifiers may be written as: {}", copycat_protocol::MODIFIER_NAMES))?;
        self.manager
            .register(hotkey)
            // Almost always another application already owns the chord.
            .map_err(|e| format!("could not be registered: {e}"))?;
        self.registered.push(hotkey);
        Ok(hotkey.id())
    }

    fn unregister_all(&mut self) {
        for hotkey in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
    }

    fn unavailable_reason(&self) -> Option<String> {
        None
    }

    fn name(&self) -> String {
        "global-hotkey".into()
    }
}

/// Bookkeeping over a backend: which id belongs to which binding, and what
/// was refused and why.
pub struct HotkeyRegistry {
    backend: Box<dyn HotkeyBackend>,
    /// Hotkey id to the index of the binding that owns it.
    bound: Vec<(u32, usize)>,
    rejected: Vec<RejectedBinding>,
    next_id: u32,
}

impl HotkeyRegistry {
    pub fn new(backend: Box<dyn HotkeyBackend>) -> Self {
        HotkeyRegistry { backend, bound: Vec::new(), rejected: Vec::new(), next_id: 1 }
    }

    /// Why shortcuts are not working, if they are not.
    pub fn unavailable_reason(&self) -> Option<String> {
        self.backend.unavailable_reason()
    }

    pub fn backend_name(&self) -> String {
        self.backend.name()
    }

    pub fn observes_leader_itself(&self) -> bool {
        self.backend.observes_leader_itself()
    }

    /// Register one trigger against a binding index. A failure is recorded and
    /// reported by `bind list`, never swallowed.
    pub fn register(&mut self, trigger: &str, binding_index: usize) {
        let proposed = self.next_id;
        self.next_id += 1;
        match self.backend.register(trigger, proposed) {
            Ok(id) => self.bound.push((id, binding_index)),
            Err(reason) => self.rejected.push(RejectedBinding { trigger: trigger.to_string(), reason }),
        }
    }

    pub fn register_leader(&mut self, trigger: &str, binding_index: usize, timeout: Duration) {
        let proposed = self.next_id;
        self.next_id += 1;
        match self.backend.register_leader(trigger, proposed, timeout) {
            Ok(id) => self.bound.push((id, binding_index)),
            Err(reason) => self.rejected.push(RejectedBinding { trigger: trigger.to_string(), reason }),
        }
    }

    /// Drop every registration, keeping the backend. Reloading rebuilds on
    /// top of this.
    pub fn reset(&mut self) {
        self.backend.unregister_all();
        self.bound.clear();
        self.rejected.clear();
    }

    pub fn binding_for(&self, id: u32) -> Option<usize> {
        self.bound.iter().find(|(hotkey, _)| *hotkey == id).map(|(_, index)| *index)
    }

    pub fn rejected(&self) -> &[RejectedBinding] {
        &self.rejected
    }

    pub fn registered_count(&self) -> usize {
        self.bound.len()
    }
}

/// Whether a chord is one the platform could register, without registering it.
///
/// Used to reject a bad leader chord before it reaches the config file, so a
/// typo cannot leave the daemon with a leader it can never arm.
pub fn parse_trigger(trigger: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        super::mac_tap::parse_chord(trigger).map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        HotKey::from_str(&copycat_protocol::normalize_trigger(trigger))
            .map(|_| ())
            .map_err(|e| format!("{e}. Modifiers may be written as: {}", copycat_protocol::MODIFIER_NAMES))
    }
}

/// Conditions under which the backend cannot work, determined without asking it.
///
/// This exists because `global-hotkey`'s Linux `register()` returns `Ok(())`
/// even when its worker thread has died — the send is discarded and the failed
/// receive is skipped — so a successful registration is not evidence of
/// anything. Checking the display ourselves is the only way to avoid reporting
/// shortcuts that will never fire.
fn backend_unusable(display_server: DisplayServer) -> Option<String> {
    match display_server {
        DisplayServer::Headless => Some(
            "no display server, so there is no session to register a shortcut with".to_string(),
        ),
        #[cfg(target_os = "linux")]
        DisplayServer::X11 | DisplayServer::Wayland => match x11rb::connect(None) {
            Ok(_) => None,
            Err(error) => Some(format!(
                "cannot reach the X server ({error}). The backend would report every \
                 registration as successful anyway, so shortcuts are reported unavailable \
                 rather than silently dead"
            )),
        },
        _ => None,
    }
}

/// Turn a backend construction failure into something a person can act on.
///
/// `global-hotkey` builds its Windows error from `io::Error::last_os_error()`
/// after a failure that does not set `errno`, so the text is whatever stale
/// value it held, and passing it along unqualified sends people looking for
/// a missing file that does not exist.
fn explain_failure(display_server: DisplayServer, error: &global_hotkey::Error) -> String {
    match display_server {
        DisplayServer::Windows => format!(
            "Windows refused to create the hidden message window the shortcut backend needs. \
             The accompanying OS error (\"{error}\") is read from the last OS error and may \
             be unrelated"
        ),
        _ => format!("the global shortcut backend could not start: {error}"),
    }
}

/// Watch for the next key press after a leader trigger.
///
/// Returns `Ok(None)` when the window closed with no key — the user thought
/// better of it, which is not an error. On macOS the event tap reads the key
/// itself and this is never reached.
pub fn observe_next_key(
    display_server: DisplayServer,
    timeout: std::time::Duration,
) -> Result<Option<String>, CoreError> {
    match display_server {
        #[cfg(target_os = "linux")]
        DisplayServer::X11 => x11_leader::observe_next_key(timeout),
        other => {
            // No observation path here, so the window never opens.
            let _ = timeout;
            Err(CoreError::new(
                ErrorKind::PlatformUnavailable,
                "leader_unavailable",
                other.leader_support().explain(other.as_str()),
            ))
        }
    }
}

#[cfg(target_os = "linux")]
mod x11_leader {
    //! Grab the keyboard briefly, read one key, let go.
    //!
    //! The grab is the point: without it the next keystroke reaches whatever
    //! application has focus, so pressing the leader would type into the user's
    //! editor. It is released on every path, including the timeout.

    use std::time::{Duration, Instant};

    use copycat_core::{CoreError, ErrorKind};
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, GrabStatus};

    fn failed(detail: impl Into<String>) -> CoreError {
        CoreError::new(ErrorKind::InputPermission, "leader_grab_failed", detail)
    }

    pub fn observe_next_key(timeout: Duration) -> Result<Option<String>, CoreError> {
        let (conn, screen) = x11rb::connect(None)
            .map_err(|e| failed(format!("cannot reach the X server: {e}")))?;
        let root = conn.setup().roots[screen].root;

        let grab = conn
            .grab_keyboard(false, root, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(|e| failed(format!("grab request failed: {e}")))?
            .reply()
            .map_err(|e| failed(format!("grab request failed: {e}")))?;
        if grab.status != GrabStatus::SUCCESS {
            return Err(failed(format!(
                "another client holds the keyboard (status {:?})",
                grab.status
            )));
        }

        let result = read_one_key(&conn, timeout);

        // Always release: leaving the keyboard grabbed would freeze the desktop.
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = conn.flush();
        result
    }

    fn read_one_key(
        conn: &x11rb::rust_connection::RustConnection,
        timeout: Duration,
    ) -> Result<Option<String>, CoreError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match conn.poll_for_event() {
                Ok(Some(Event::KeyPress(event))) => {
                    let shifted = event.state.contains(x11rb::protocol::xproto::KeyButMask::SHIFT);
                    return Ok(keysym_to_string(conn, event.detail, shifted));
                }
                Ok(Some(_)) => continue,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(failed(format!("event stream failed: {e}"))),
            }
        }
        Ok(None)
    }

    /// Resolve a keycode to the character a binding would be written with.
    ///
    /// Bindings are configured as characters (`s`, `S`, `2`), so the shift
    /// level matters: `leader S` and `leader s` are two different bindings by
    /// design (§3.6).
    fn keysym_to_string(
        conn: &x11rb::rust_connection::RustConnection,
        keycode: u8,
        shifted: bool,
    ) -> Option<String> {
        let setup = conn.setup();
        let mapping = conn
            .get_keyboard_mapping(keycode, 1)
            .ok()?
            .reply()
            .ok()?;
        let _ = setup;

        let per_code = mapping.keysyms_per_keycode as usize;
        let group = mapping.keysyms.chunks(per_code.max(1)).next()?;
        let keysym = *group.get(usize::from(shifted)).filter(|k| **k != 0).or(group.first())?;

        // Latin-1 keysyms are their own character codes; anything else is not
        // something a binding is written with.
        match keysym {
            0x20..=0x7e => char::from_u32(keysym).map(|c| c.to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(reason: &str) -> Box<dyn HotkeyBackend> {
        Box::new(NoBackend { reason: reason.into() })
    }

    #[test]
    fn a_headless_session_has_no_shortcut_backend_and_says_why() {
        let registry = HotkeyRegistry::new(backend_for(DisplayServer::Headless));
        let reason = registry.unavailable_reason().expect("headless has no backend");
        assert!(reason.contains("no display server"), "{reason}");
        assert_eq!(registry.registered_count(), 0);
    }

    #[test]
    fn a_binding_registered_without_a_backend_is_rejected_with_that_reason() {
        // Never silently accepted: a shortcut that cannot fire has to show up
        // in `bind list` saying so.
        let mut registry = HotkeyRegistry::new(none("no display server"));
        registry.register("ctrl+alt+v", 0);

        assert_eq!(registry.registered_count(), 0);
        assert_eq!(registry.rejected().len(), 1);
        assert!(registry.rejected()[0].reason.contains("no display server"));
    }

    #[test]
    fn reset_forgets_registrations_and_rejections_but_keeps_the_backend() {
        let mut registry = HotkeyRegistry::new(none("nothing here"));
        registry.register("ctrl+alt+v", 0);
        registry.reset();
        assert!(registry.rejected().is_empty());
        assert_eq!(registry.backend_name(), "unavailable");
    }

    #[test]
    fn a_windows_backend_failure_is_explained_rather_than_passed_through() {
        // global-hotkey builds the Windows error from io::Error::last_os_error()
        // after a failure that never set errno, so the raw text sends people
        // hunting a file that does not exist.
        let error = global_hotkey::Error::OsError(std::io::Error::from_raw_os_error(2));
        let explained = explain_failure(DisplayServer::Windows, &error);
        assert!(explained.contains("message window"), "{explained}");
        assert!(explained.contains("may be unrelated"), "{explained}");
    }
}
