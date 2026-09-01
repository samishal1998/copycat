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
    /// Why there is no usable backend, when there isn't one. Carried so
    /// `doctor` can say what happened rather than just that nothing works.
    unavailable: Option<String>,
}

impl HotkeyRegistry {
    pub fn new(display_server: DisplayServer) -> Self {
        // Ask what we can determine ourselves first. The backend reports
        // success in situations where it cannot actually deliver anything, so
        // taking its word for it would mean claiming a capability we do not
        // have.
        if let Some(reason) = backend_unusable(display_server) {
            return HotkeyRegistry::without_backend(reason);
        }

        match GlobalHotKeyManager::new() {
            Ok(manager) => HotkeyRegistry {
                manager: Some(manager),
                bound: Vec::new(),
                rejected: Vec::new(),
                unavailable: None,
            },
            Err(error) => HotkeyRegistry::without_backend(explain_failure(display_server, &error)),
        }
    }

    fn without_backend(reason: String) -> Self {
        HotkeyRegistry {
            manager: None,
            bound: Vec::new(),
            rejected: Vec::new(),
            unavailable: Some(reason),
        }
    }

    /// Why shortcuts are not working, if they are not.
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Register one trigger against a binding index. A failure is recorded and
    /// reported by `bind list`, never swallowed.
    pub fn register(&mut self, trigger: &str, binding_index: usize) {
        let Some(manager) = &self.manager else {
            self.rejected.push(RejectedBinding {
                trigger: trigger.to_string(),
                reason: self
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| "no global shortcut backend".into()),
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

/// Whether a chord is one the platform could register, without registering it.
///
/// Used to reject a bad leader chord before it reaches the config file, so a
/// typo cannot leave the daemon with a leader it can never arm.
pub fn parse_trigger(trigger: &str) -> Result<(), String> {
    HotKey::from_str(trigger).map(|_| ()).map_err(|e| format!("{e}"))
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
/// `global-hotkey` builds its macOS and Windows errors from
/// `io::Error::last_os_error()` after failures that do not set `errno` — a
/// Carbon `OSStatus` and a null window handle respectively. The resulting text
/// is whatever stale value `errno` happened to hold, and passing it along
/// unqualified sends people looking for a missing file that does not exist.
fn explain_failure(display_server: DisplayServer, error: &global_hotkey::Error) -> String {
    match display_server {
        DisplayServer::MacOs => format!(
            "macOS refused to install the hotkey event handler. The usual cause is a daemon \
             started outside an application bundle, which has no Carbon application event \
             target to attach to. The accompanying OS error (\"{error}\") is read from errno \
             after a non-errno failure and is not meaningful"
        ),
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
/// better of it, which is not an error.
pub fn observe_next_key(
    display_server: DisplayServer,
    timeout: std::time::Duration,
) -> Result<Option<String>, CoreError> {
    match display_server {
        #[cfg(target_os = "linux")]
        DisplayServer::X11 => x11_leader::observe_next_key(timeout),
        #[cfg(target_os = "macos")]
        DisplayServer::MacOs => macos_leader::observe_next_key(timeout),
        other => {
            // No observation path on this platform, so the window never opens.
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


#[cfg(target_os = "macos")]
mod macos_leader {
    //! A short-lived CGEventTap, which is what the PRD's "narrowly scoped event
    //! tap" (§7.2) means in practice.
    //!
    //! Two things force raw FFI here rather than `core_graphics::CGEventTap`.
    //! The safe wrapper's callback returns `Option<CGEvent>`, and `None` passes
    //! the original event through — it has no way to express "drop this". A
    //! leader that cannot drop the key it just read would start a stack *and*
    //! type the letter into whatever has focus, which is worse than having no
    //! leader at all. The C callback returns null to consume, so that is what
    //! this does.
    //!
    //! The tap is created, run for at most the leader timeout, and torn down.
    //! Exactly one key is ever consumed: the flag is set before the send, so a
    //! second key arriving in the same run-loop pass is passed through
    //! untouched. That bound matters — a tap that swallowed keystrokes
    //! indefinitely would be a very bad thing to leave on a person's machine.

    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use copycat_core::{CoreError, ErrorKind};
    use core_foundation::base::TCFType;
    use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
    use core_foundation::runloop::{CFRunLoop, CFRunLoopRunResult, kCFRunLoopCommonModes};

    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *const c_void;
    type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef;

    // kCGHIDEventTap / kCGHeadInsertEventTap / kCGEventTapOptionDefault.
    const TAP_HID: u32 = 0;
    const PLACE_HEAD_INSERT: u32 = 0;
    const OPTION_DEFAULT: u32 = 0;
    const EVENT_KEY_DOWN: u32 = 10;
    const FIELD_KEYCODE: u32 = 9;
    const FLAG_SHIFT: u64 = 0x0002_0000;

    // The system disables a tap that misbehaves and tells us through these two
    // pseudo-events; they carry no keycode and must be passed straight back.
    const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
    }

    struct TapState {
        tx: mpsc::Sender<(u16, bool)>,
        taken: AtomicBool,
    }

    unsafe extern "C" fn on_event(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef {
        if etype == TAP_DISABLED_BY_TIMEOUT || etype == TAP_DISABLED_BY_USER_INPUT {
            return event;
        }
        let state = unsafe { &*(user_info as *const TapState) };

        // Claim the slot before reading, so only the first key is ever
        // swallowed no matter how the run loop schedules things.
        if state.taken.swap(true, Ordering::SeqCst) {
            return event;
        }

        let keycode = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) } as u16;
        let shifted = unsafe { CGEventGetFlags(event) } & FLAG_SHIFT != 0;
        let _ = state.tx.send((keycode, shifted));

        // Null consumes the event: the leader's key must not also reach the
        // application that has focus.
        std::ptr::null_mut()
    }

    fn denied(detail: impl Into<String>) -> CoreError {
        CoreError::new(ErrorKind::InputPermission, "leader_tap_denied", detail)
    }

    pub fn observe_next_key(timeout: Duration) -> Result<Option<String>, CoreError> {
        let (tx, rx) = mpsc::channel();
        // Outlives the tap; the tap is torn down before this function returns.
        let state = Box::new(TapState { tx, taken: AtomicBool::new(false) });

        let port = unsafe {
            CGEventTapCreate(
                TAP_HID,
                PLACE_HEAD_INSERT,
                OPTION_DEFAULT,
                1u64 << EVENT_KEY_DOWN,
                on_event,
                &*state as *const TapState as *mut c_void,
            )
        };
        if port.is_null() {
            return Err(denied(
                "macOS refused the keyboard event tap. Grant Accessibility permission to the \
                 program running copycatd (System Settings, Privacy & Security, Accessibility), \
                 then restart the daemon",
            ));
        }

        let port = unsafe { CFMachPort::wrap_under_create_rule(port) };
        let source = port
            .create_runloop_source(0)
            .map_err(|_| denied("could not attach the event tap to a run loop"))?;

        let run_loop = CFRunLoop::get_current();
        unsafe {
            run_loop.add_source(&source, kCFRunLoopCommonModes);
            CGEventTapEnable(port.as_concrete_TypeRef(), true);
        }

        // Returns as soon as one event is handled, or when the window closes.
        let outcome = CFRunLoop::run_in_mode(unsafe { kCFRunLoopCommonModes }, timeout, true);

        // Always tear down, on every path: the tap is disabled when the port is
        // released, and leaving it live would keep intercepting keys.
        unsafe {
            CGEventTapEnable(port.as_concrete_TypeRef(), false);
            run_loop.remove_source(&source, kCFRunLoopCommonModes);
        }
        drop(port);
        drop(state);

        match outcome {
            CFRunLoopRunResult::HandledSource => Ok(rx
                .try_recv()
                .ok()
                .and_then(|(keycode, shifted)| character_for(keycode, shifted))),
            _ => Ok(None),
        }
    }

    /// Virtual keycode to the character a binding is written with.
    ///
    /// This is the ANSI layout. macOS exposes the layout-correct answer through
    /// `CGEventKeyboardGetUnicodeString`, which `core-graphics` does not wrap,
    /// and reaching it means more unverified FFI on a path that already cannot
    /// be tested here. A non-US layout will therefore resolve some leader keys
    /// by physical position rather than by printed label.
    fn character_for(keycode: u16, shifted: bool) -> Option<String> {
        // (keycode, unshifted, shifted) from Carbon's Events.h kVK_ANSI_* set.
        const KEYS: &[(u16, char, char)] = &[
            (0x00, 'a', 'A'), (0x01, 's', 'S'), (0x02, 'd', 'D'), (0x03, 'f', 'F'),
            (0x04, 'h', 'H'), (0x05, 'g', 'G'), (0x06, 'z', 'Z'), (0x07, 'x', 'X'),
            (0x08, 'c', 'C'), (0x09, 'v', 'V'), (0x0B, 'b', 'B'), (0x0C, 'q', 'Q'),
            (0x0D, 'w', 'W'), (0x0E, 'e', 'E'), (0x0F, 'r', 'R'), (0x10, 'y', 'Y'),
            (0x11, 't', 'T'), (0x12, '1', '!'), (0x13, '2', '@'), (0x14, '3', '#'),
            (0x15, '4', '$'), (0x16, '6', '^'), (0x17, '5', '%'), (0x18, '=', '+'),
            (0x19, '9', '('), (0x1A, '7', '&'), (0x1B, '-', '_'), (0x1C, '8', '*'),
            (0x1D, '0', ')'), (0x1E, ']', '}'), (0x1F, 'o', 'O'), (0x20, 'u', 'U'),
            (0x21, '[', '{'), (0x22, 'i', 'I'), (0x23, 'p', 'P'), (0x25, 'l', 'L'),
            (0x26, 'j', 'J'), (0x27, '\'', '"'), (0x28, 'k', 'K'), (0x29, ';', ':'),
            (0x2A, '\\', '|'), (0x2B, ',', '<'), (0x2C, '/', '?'), (0x2D, 'n', 'N'),
            (0x2E, 'm', 'M'), (0x2F, '.', '>'), (0x32, '`', '~'),
        ];
        KEYS.iter()
            .find(|(code, _, _)| *code == keycode)
            .map(|(_, plain, upper)| if shifted { *upper } else { *plain }.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::character_for;

        #[test]
        fn letters_and_digits_resolve_with_the_shift_level() {
            assert_eq!(character_for(0x01, false).as_deref(), Some("s"));
            assert_eq!(character_for(0x01, true).as_deref(), Some("S"));
            assert_eq!(character_for(0x13, false).as_deref(), Some("2"));
            assert_eq!(character_for(0x13, true).as_deref(), Some("@"));
        }

        #[test]
        fn an_unmapped_key_resolves_to_nothing_rather_than_a_wrong_binding() {
            assert_eq!(character_for(0x7E, false), None); // arrow up
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_session_has_no_shortcut_backend_and_says_why() {
        let registry = HotkeyRegistry::new(DisplayServer::Headless);
        let reason = registry.unavailable_reason().expect("headless has no backend");
        assert!(reason.contains("no display server"), "{reason}");
        assert_eq!(registry.registered_count(), 0);
    }

    #[test]
    fn a_binding_registered_without_a_backend_is_rejected_with_that_reason() {
        // Never silently accepted: a shortcut that cannot fire has to show up
        // in `bind list` saying so.
        let mut registry = HotkeyRegistry::new(DisplayServer::Headless);
        registry.register("ctrl+alt+v", 0);

        assert_eq!(registry.registered_count(), 0);
        assert_eq!(registry.rejected().len(), 1);
        assert!(registry.rejected()[0].reason.contains("no display server"));
    }

    #[test]
    fn a_backend_failure_is_explained_rather_than_passed_through() {
        // global-hotkey builds macOS and Windows errors from
        // io::Error::last_os_error() after failures that never set errno, so
        // the raw text sends people hunting a file that does not exist.
        let error = global_hotkey::Error::OsError(std::io::Error::from_raw_os_error(2));
        let raw = format!("{error}");
        assert!(raw.contains("No such file or directory"), "{raw}");

        let explained = explain_failure(DisplayServer::MacOs, &error);
        assert!(explained.contains("application bundle"), "{explained}");
        assert!(explained.contains("not meaningful"), "{explained}");
    }
}
