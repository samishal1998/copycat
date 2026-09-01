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
//! So the leader is implemented for X11 here and reported as unavailable
//! elsewhere. Pretending otherwise would produce the worst outcome: a key that
//! sometimes does nothing for reasons the user cannot see.

use std::str::FromStr;

use copycat_core::{CoreError, ErrorKind};
use copycat_protocol::RejectedBinding;
use global_hotkey::{GlobalHotKeyManager, hotkey::HotKey};

use super::DisplayServer;

pub struct HotkeyRegistry {
    manager: Option<GlobalHotKeyManager>,
    /// Hotkey id to the index of the binding that owns it.
    bound: Vec<(u32, usize)>,
    rejected: Vec<RejectedBinding>,
}

impl HotkeyRegistry {
    /// A registry that owns the platform manager, or one that rejects
    /// everything with a reason if the platform would not give us one.
    pub fn new() -> Self {
        match GlobalHotKeyManager::new() {
            Ok(manager) => HotkeyRegistry { manager: Some(manager), bound: Vec::new(), rejected: Vec::new() },
            Err(error) => HotkeyRegistry {
                manager: None,
                bound: Vec::new(),
                rejected: vec![RejectedBinding {
                    trigger: "*".into(),
                    reason: format!("no global shortcut backend: {error}"),
                }],
            },
        }
    }

    pub fn available(&self) -> bool {
        self.manager.is_some()
    }

    /// Register one trigger against a binding index. A failure is recorded and
    /// reported by `bind list`, never swallowed.
    pub fn register(&mut self, trigger: &str, binding_index: usize) {
        let Some(manager) = &self.manager else {
            self.rejected.push(RejectedBinding {
                trigger: trigger.to_string(),
                reason: "no global shortcut backend on this platform".into(),
            });
            return;
        };

        let hotkey = match HotKey::from_str(trigger) {
            Ok(hotkey) => hotkey,
            Err(error) => {
                self.rejected.push(RejectedBinding {
                    trigger: trigger.to_string(),
                    reason: format!("unparseable: {error}"),
                });
                return;
            }
        };

        match manager.register(hotkey) {
            Ok(()) => self.bound.push((hotkey.id(), binding_index)),
            Err(error) => self.rejected.push(RejectedBinding {
                trigger: trigger.to_string(),
                // Almost always another application already owns the chord.
                reason: format!("could not be registered: {error}"),
            }),
        }
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

/// Watch for the next key press after a leader trigger.
///
/// Returns `Ok(None)` when the window closed with no key — the user thought
/// better of it, which is not an error.
pub fn observe_next_key(
    display_server: DisplayServer,
    timeout: std::time::Duration,
) -> Result<Option<String>, CoreError> {
    match display_server {
        #[cfg(target_os = "linux")]
        DisplayServer::X11 => x11_leader::observe_next_key(timeout),
        other => Err(CoreError::new(
            ErrorKind::PlatformUnavailable,
            "leader_unavailable",
            format!(
                "leader sequences need to observe the next key press, which {} does not offer; \
                 use direct hotkeys instead",
                other.as_str()
            ),
        )),
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
