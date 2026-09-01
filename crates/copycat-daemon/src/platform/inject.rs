//! Sending the platform's paste chord to the focused application.
//!
//! This is the step that makes Copycat feel like a clipboard rather than a
//! database: the value is written, then the normal paste keystroke is
//! delivered so the focused app pastes it itself (§4.5).
//!
//! None of these has been run. All three are written against their documented
//! APIs and compile-checked; the machine this was built on has no display.
//! ADR-015 makes the adapter contract suite the gate, and no platform has been
//! through it, so `doctor` reports every one of them as unproven rather than
//! implying otherwise.

use copycat_core::{CoreError, ErrorKind};

use super::{PasteInjector, Result};

pub fn unavailable(detail: impl Into<String>) -> CoreError {
    CoreError::new(ErrorKind::PlatformUnavailable, "injection_unavailable", detail)
}

#[cfg(target_os = "linux")]
pub fn system_injector() -> Result<Box<dyn PasteInjector>> {
    Ok(Box::new(x11::X11Injector::new()?))
}

#[cfg(target_os = "macos")]
pub fn system_injector() -> Result<Box<dyn PasteInjector>> {
    Ok(Box::new(macos::MacInjector))
}

#[cfg(target_os = "windows")]
pub fn system_injector() -> Result<Box<dyn PasteInjector>> {
    Ok(Box::new(windows::WindowsInjector))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn system_injector() -> Result<Box<dyn PasteInjector>> {
    Err(unavailable("paste injection is not implemented for this platform"))
}

#[cfg(target_os = "linux")]
pub mod x11 {
    //! XTEST fake input. Pure Rust through `x11rb`, so no X11 headers are
    //! needed to build — which matters because the daemon must build on
    //! machines that will never run a display.

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;

    use super::{PasteInjector, Result, unavailable};

    /// X11 keysyms for the keys the paste chord needs.
    const XK_V: u32 = 0x0076;
    const XK_CONTROL_L: u32 = 0xffe3;
    const XK_SHIFT_L: u32 = 0xffe1;

    pub struct X11Injector {
        conn: RustConnection,
        root: u32,
        control: u8,
        key_v: u8,
        shift: u8,
    }

    impl X11Injector {
        pub fn new() -> Result<Self> {
            let (conn, screen_index) = x11rb::connect(None)
                .map_err(|e| unavailable(format!("cannot reach the X server: {e}")))?;
            let root = conn.setup().roots[screen_index].root;

            let control = keycode_for(&conn, XK_CONTROL_L)?;
            let key_v = keycode_for(&conn, XK_V)?;
            let shift = keycode_for(&conn, XK_SHIFT_L).unwrap_or(0);

            Ok(X11Injector { conn, root, control, key_v, shift })
        }

        fn key(&self, event: u8, keycode: u8) -> Result<()> {
            self.conn
                .xtest_fake_input(event, keycode, 0, self.root, 0, 0, 0)
                .map_err(|e| unavailable(format!("XTEST rejected the event: {e}")))?;
            Ok(())
        }
    }

    fn keycode_for(conn: &RustConnection, keysym: u32) -> Result<u8> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let count = setup.max_keycode - min + 1;
        let mapping = conn
            .get_keyboard_mapping(min, count)
            .map_err(|e| unavailable(format!("cannot read the keyboard map: {e}")))?
            .reply()
            .map_err(|e| unavailable(format!("cannot read the keyboard map: {e}")))?;

        let per_code = mapping.keysyms_per_keycode as usize;
        if per_code == 0 {
            return Err(unavailable("the X server reported an empty keyboard map"));
        }
        mapping
            .keysyms
            .chunks(per_code)
            .position(|group| group.contains(&keysym))
            .map(|index| min + index as u8)
            .ok_or_else(|| unavailable(format!("keysym {keysym:#06x} is not on this keyboard")))
    }

    impl PasteInjector for X11Injector {
        fn inject(&mut self) -> Result<()> {
            // Release order mirrors press order in reverse so no modifier is
            // left latched if a step fails midway.
            self.key(KEY_PRESS_EVENT, self.control)?;
            self.key(KEY_PRESS_EVENT, self.key_v)?;
            self.key(KEY_RELEASE_EVENT, self.key_v)?;
            self.key(KEY_RELEASE_EVENT, self.control)?;
            self.conn
                .flush()
                .map_err(|e| unavailable(format!("cannot flush to the X server: {e}")))?;
            Ok(())
        }

        fn name(&self) -> String {
            let _ = self.shift;
            "x11-xtest".into()
        }
    }
}

#[cfg(target_os = "macos")]
pub mod macos {
    //! `CGEvent` keyboard synthesis. Requires Accessibility permission; when it
    //! is missing the events are silently dropped by the OS, which is exactly
    //! the failure `doctor` exists to name.

    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::{PasteInjector, Result, unavailable};

    /// `kVK_ANSI_V` from Carbon's `Events.h`.
    const KEY_V: u16 = 0x09;

    pub struct MacInjector;

    impl PasteInjector for MacInjector {
        fn inject(&mut self) -> Result<()> {
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .map_err(|()| unavailable("cannot create a CGEventSource"))?;

            for down in [true, false] {
                let event = CGEvent::new_keyboard_event(source.clone(), KEY_V, down)
                    .map_err(|()| unavailable("cannot create a keyboard event"))?;
                event.set_flags(CGEventFlags::CGEventFlagCommand);
                event.post(CGEventTapLocation::HID);
            }
            Ok(())
        }

        fn name(&self) -> String {
            "macos-cgevent".into()
        }
    }
}

#[cfg(target_os = "windows")]
pub mod windows {
    //! `SendInput` with the Ctrl+V chord.

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL,
    };

    use super::{PasteInjector, Result, unavailable};

    const VK_V: VIRTUAL_KEY = 0x56;

    pub struct WindowsInjector;

    fn key(code: VIRTUAL_KEY, up: bool) -> INPUT {
        let mut input: INPUT = unsafe { std::mem::zeroed() };
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: code,
            wScan: 0,
            dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
            time: 0,
            dwExtraInfo: 0,
        };
        input
    }

    impl PasteInjector for WindowsInjector {
        fn inject(&mut self) -> Result<()> {
            let inputs = [
                key(VK_CONTROL, false),
                key(VK_V, false),
                key(VK_V, true),
                key(VK_CONTROL, true),
            ];
            let sent = unsafe {
                SendInput(
                    inputs.len() as u32,
                    inputs.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                )
            };
            if sent as usize != inputs.len() {
                return Err(unavailable(
                    "SendInput was blocked, most likely by UIPI or an elevated foreground window",
                ));
            }
            Ok(())
        }

        fn name(&self) -> String {
            "windows-sendinput".into()
        }
    }
}
