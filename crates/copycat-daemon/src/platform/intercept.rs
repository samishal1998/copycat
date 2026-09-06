//! Intercepting the user's own paste chord while a mode is active (R21).
//!
//! This is the product's central mechanism. With a stack running, Ctrl/Cmd+V
//! in any application should pop it - not a second chord the user has to
//! learn, the paste key they already press. The pattern is the same on every
//! platform: see the chord before the application does, write the next item
//! to the clipboard synchronously, then let the original keystroke continue so
//! the application pastes it itself. No synthetic events, so nothing to
//! suppress and no recursion.
//!
//! When no mode is active nothing is hooked at all (§3.1: Copycat is
//! invisible), so the cost of the hook is only paid while a session is live.
//!
//! macOS is not here: its interception lives in the daemon's unified event
//! tap (`mac_tap`), which also serves hotkeys and the leader.
//!
//! The [`Handler`] is how the platform thread asks the daemon to perform the
//! write. It blocks until the write is done, because the keystroke must not be
//! released to the application before the clipboard holds the new value; a
//! short timeout keeps a stuck daemon from freezing the user's keyboard.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::DisplayServer;

/// Performs the mode paste and returns once the clipboard holds the item.
pub type Handler = Arc<dyn Fn() + Send + Sync>;

/// How long the platform thread waits for the daemon before releasing the
/// keystroke anyway. Long enough for a clipboard write, short enough that a
/// wedged daemon is an annoyance rather than a frozen keyboard.
pub const HANDLER_TIMEOUT: Duration = Duration::from_millis(300);

pub trait PasteInterceptor: Send {
    /// Hook or unhook the paste chord. Idempotent, called after every event.
    fn set_active(&mut self, active: bool);
    fn name(&self) -> String;
    /// Why interception cannot work here, when it cannot.
    fn unavailable_reason(&self) -> Option<String>;
}

pub fn for_platform(display_server: DisplayServer, handler: Handler) -> Box<dyn PasteInterceptor> {
    match display_server {
        #[cfg(target_os = "linux")]
        DisplayServer::X11 => Box::new(x11::X11Interceptor::new(handler)),
        #[cfg(target_os = "windows")]
        DisplayServer::Windows => Box::new(windows::WindowsInterceptor::new(handler)),
        DisplayServer::Wayland => Box::new(Unavailable {
            reason: "a Wayland client cannot see other applications' key presses; bind a \
                     global shortcut to `paste.mode` instead"
                .into(),
        }),
        DisplayServer::Headless => Box::new(Unavailable {
            reason: "no display server, so there is no keyboard to intercept".into(),
        }),
        #[allow(unreachable_patterns)]
        other => Box::new(Unavailable {
            reason: format!("paste interception is not implemented for {}", other.as_str()),
        }),
    }
}

struct Unavailable {
    reason: String,
}

impl PasteInterceptor for Unavailable {
    fn set_active(&mut self, _active: bool) {}

    fn name(&self) -> String {
        "unavailable".into()
    }

    fn unavailable_reason(&self) -> Option<String> {
        Some(self.reason.clone())
    }
}

/// Shared between a platform thread and its controller.
struct Shared {
    active: AtomicBool,
    handler: Handler,
}

#[cfg(target_os = "linux")]
mod x11 {
    //! A passive grab on Ctrl+V with the keyboard in synchronous mode. The
    //! server freezes keyboard delivery until we call `allow_events`, and
    //! `REPLAY_KEYBOARD` then re-delivers the very same key press to whichever
    //! window would have received it - by which time the clipboard already
    //! holds the next item.

    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{Allow, ConnectionExt as _, GrabMode, ModMask};

    use super::{Handler, PasteInterceptor, Shared};
    use crate::platform::inject::x11::{XK_V, keycode_for};

    pub struct X11Interceptor {
        shared: Arc<Shared>,
        started: bool,
        failure: Option<String>,
    }

    impl X11Interceptor {
        pub fn new(handler: Handler) -> Self {
            X11Interceptor {
                shared: Arc::new(Shared { active: false.into(), handler }),
                started: false,
                failure: None,
            }
        }
    }

    impl PasteInterceptor for X11Interceptor {
        fn set_active(&mut self, active: bool) {
            self.shared.active.store(active, Ordering::SeqCst);
            if active && !self.started {
                self.started = true;
                let shared = Arc::clone(&self.shared);
                std::thread::spawn(move || {
                    if let Err(error) = run(shared) {
                        tracing::warn!(error, "paste interception stopped");
                    }
                });
            }
        }

        fn name(&self) -> String {
            "x11-grab".into()
        }

        fn unavailable_reason(&self) -> Option<String> {
            self.failure.clone()
        }
    }

    /// Lock modifiers must not defeat the grab, so it is taken for every
    /// combination of Caps Lock and Num Lock as well.
    fn lock_variants() -> [ModMask; 4] {
        [ModMask::default(), ModMask::LOCK, ModMask::M2, ModMask::LOCK | ModMask::M2]
    }

    fn run(shared: Arc<Shared>) -> Result<(), String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("cannot reach X: {e}"))?;
        let root = conn.setup().roots[screen].root;
        let key_v = keycode_for(&conn, XK_V).map_err(|e| e.message)?;
        let mut grabbed = false;

        loop {
            let active = shared.active.load(Ordering::SeqCst);
            if active != grabbed {
                for lock in lock_variants() {
                    let mods = ModMask::CONTROL | lock;
                    if active {
                        conn.grab_key(false, root, mods, key_v, GrabMode::ASYNC, GrabMode::SYNC)
                            .map_err(|e| format!("grab failed: {e}"))?;
                    } else {
                        conn.ungrab_key(key_v, root, mods).map_err(|e| format!("ungrab failed: {e}"))?;
                    }
                }
                conn.flush().map_err(|e| format!("{e}"))?;
                grabbed = active;
            }

            match conn.poll_for_event() {
                Ok(Some(Event::KeyPress(press))) if press.detail == key_v => {
                    // The keyboard is frozen here. Write, then replay so the
                    // application receives the same Ctrl+V it was going to.
                    (shared.handler)();
                    let _ = conn.allow_events(Allow::REPLAY_KEYBOARD, press.time);
                    let _ = conn.flush();
                }
                Ok(Some(Event::KeyRelease(release))) => {
                    // Releases are grabbed too; let them through unchanged.
                    let _ = conn.allow_events(Allow::ASYNC_KEYBOARD, release.time);
                    let _ = conn.flush();
                }
                Ok(Some(_)) => {}
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(format!("event stream failed: {e}")),
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    //! A low-level keyboard hook. Returning `CallNextHookEx` passes the key
    //! on; by then the clipboard already holds the next item. The hook needs a
    //! thread that pumps messages, so the thread polls rather than blocks.

    use std::sync::atomic::Ordering;
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, KBDLLHOOKSTRUCT, MSG, PM_REMOVE, PeekMessageW,
        SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_SYSKEYDOWN,
    };

    use super::{Handler, PasteInterceptor, Shared};

    const VK_V: u32 = 0x56;

    /// Hook procedures take no user data, so the shared state is a global.
    static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

    pub struct WindowsInterceptor {
        shared: Arc<Shared>,
        started: bool,
    }

    impl WindowsInterceptor {
        pub fn new(handler: Handler) -> Self {
            WindowsInterceptor { shared: Arc::new(Shared { active: false.into(), handler }), started: false }
        }
    }

    impl PasteInterceptor for WindowsInterceptor {
        fn set_active(&mut self, active: bool) {
            self.shared.active.store(active, Ordering::SeqCst);
            if active && !self.started {
                self.started = true;
                let _ = SHARED.set(Arc::clone(&self.shared));
                std::thread::spawn(run);
            }
        }

        fn name(&self) -> String {
            "windows-ll-hook".into()
        }

        fn unavailable_reason(&self) -> Option<String> {
            None
        }
    }

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && (wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN) {
            let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let control_down = unsafe { GetAsyncKeyState(VK_CONTROL as i32) } as u16 & 0x8000 != 0;
            if info.vkCode == VK_V
                && control_down
                && let Some(shared) = SHARED.get()
                && shared.active.load(Ordering::SeqCst)
            {
                (shared.handler)();
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    fn run() {
        let handle = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), std::ptr::null_mut(), 0) };
        if handle.is_null() {
            tracing::warn!("could not install the keyboard hook");
            return;
        }
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        #[allow(unreachable_code)]
        unsafe {
            UnhookWindowsHookEx(handle);
        }
    }
}
