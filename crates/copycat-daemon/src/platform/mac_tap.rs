//! One `CGEventTap` for everything macOS needs from the keyboard.
//!
//! `global-hotkey` registers macOS shortcuts through Carbon: an event handler
//! on the application event target, dispatched by the application's main run
//! loop. A daemon has neither, so `InstallEventHandler` fails - and had it
//! succeeded, nothing would ever pump the events. The mechanism macOS does
//! offer a daemon is an event tap, which is what the paste interceptor and the
//! leader already used. This folds all three into one tap: one thread, one
//! Accessibility permission, no Carbon.
//!
//! On every key-down the callback decides, in order: is a leader sequence
//! armed (then this key is it), does the chord match the leader or a hotkey
//! (consume and act), is it the paste chord while a mode is active (write,
//! then pass through). Anything else passes through untouched.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::mach_port::{CFMachPort, CFMachPortRef};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};

use super::hotkey::HotkeyBackend;
use super::intercept::{Handler, PasteInterceptor};

type CGEventRef = *mut c_void;
type CGEventTapProxy = *const c_void;
type CGEventTapCallBack =
    unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

const TAP_HID: u32 = 0;
const PLACE_HEAD_INSERT: u32 = 0;
const OPTION_DEFAULT: u32 = 0;
const EVENT_KEY_DOWN: u32 = 10;
const FIELD_KEYCODE: u32 = 9;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

pub const FLAG_SHIFT: u64 = 0x0002_0000;
pub const FLAG_CONTROL: u64 = 0x0004_0000;
pub const FLAG_OPTION: u64 = 0x0008_0000;
pub const FLAG_COMMAND: u64 = 0x0010_0000;
/// The modifiers a chord is compared on. Caps Lock, Fn and the numeric-pad
/// flag are deliberately outside it.
const MOD_MASK: u64 = FLAG_SHIFT | FLAG_CONTROL | FLAG_OPTION | FLAG_COMMAND;

/// `kVK_ANSI_V`.
const KEY_V: i64 = 0x09;

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

/// What the tap tells the daemon. Closures rather than a channel type, so this
/// module does not depend on the server's event enum.
pub struct Events {
    pub on_hotkey: Arc<dyn Fn(u32) + Send + Sync>,
    pub on_leader_key: Arc<dyn Fn(Option<String>) + Send + Sync>,
    pub on_paste_chord: Handler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub flags: u64,
    pub keycode: i64,
}

struct Registered {
    chord: Chord,
    id: u32,
    is_leader: bool,
}

struct Shared {
    chords: Mutex<Vec<Registered>>,
    armed_at: Mutex<Option<Instant>>,
    leader_timeout_ms: AtomicU64,
    intercept_active: AtomicBool,
    started: AtomicBool,
    /// The tap's mach port, so a tap the system disabled can be re-enabled
    /// from inside the callback.
    port: AtomicUsize,
    failure: Mutex<Option<String>>,
    events: Events,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The tap and its two faces.
pub struct MacTap(Arc<Shared>);

impl MacTap {
    pub fn new(events: Events) -> Self {
        MacTap(Arc::new(Shared {
            chords: Mutex::new(Vec::new()),
            armed_at: Mutex::new(None),
            leader_timeout_ms: AtomicU64::new(1200),
            intercept_active: AtomicBool::new(false),
            started: AtomicBool::new(false),
            port: AtomicUsize::new(0),
            failure: Mutex::new(None),
            events,
        }))
    }

    pub fn hotkeys(&self) -> MacTapHotkeys {
        MacTapHotkeys(Arc::clone(&self.0))
    }

    pub fn interceptor(&self) -> MacTapInterceptor {
        MacTapInterceptor(Arc::clone(&self.0))
    }
}

/// The tap thread is started the first time anything needs it, so a daemon
/// with no bindings and no session never asks for Accessibility at all.
fn ensure_started(shared: &Arc<Shared>) {
    if shared.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        if let Err(error) = run(&shared) {
            tracing::warn!(error, "macOS event tap stopped");
            *lock(&shared.failure) = Some(error);
        }
    });
}

fn run(shared: &Arc<Shared>) -> Result<(), String> {
    let port = unsafe {
        CGEventTapCreate(
            TAP_HID,
            PLACE_HEAD_INSERT,
            OPTION_DEFAULT,
            1u64 << EVENT_KEY_DOWN,
            on_event,
            Arc::as_ptr(shared) as *mut c_void,
        )
    };
    if port.is_null() {
        return Err("macOS refused the keyboard event tap. Grant Accessibility permission to \
                    the program running copycatd (System Settings, Privacy & Security, \
                    Accessibility), then restart the daemon"
            .into());
    }
    shared.port.store(port as usize, Ordering::SeqCst);

    let port = unsafe { CFMachPort::wrap_under_create_rule(port) };
    let source = port
        .create_runloop_source(0)
        .map_err(|_| "could not attach the event tap to a run loop".to_string())?;
    let run_loop = CFRunLoop::get_current();
    unsafe {
        run_loop.add_source(&source, kCFRunLoopCommonModes);
        CGEventTapEnable(port.as_concrete_TypeRef(), true);
    }

    // Lives for the daemon's lifetime. The callback decides per key whether
    // there is anything to do, so an idle tap costs one flag check.
    loop {
        let _ = CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopCommonModes },
            Duration::from_secs(60),
            false,
        );
    }
}

unsafe extern "C" fn on_event(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let shared = unsafe { &*(user_info as *const Shared) };

    // The system disables a tap whose callback was too slow. Re-enable it
    // rather than silently losing every shortcut from then on.
    if etype == TAP_DISABLED_BY_TIMEOUT || etype == TAP_DISABLED_BY_USER_INPUT {
        let port = shared.port.load(Ordering::SeqCst);
        if port != 0 {
            unsafe { CGEventTapEnable(port as CFMachPortRef, true) };
        }
        return event;
    }

    let keycode = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
    let flags = unsafe { CGEventGetFlags(event) };
    let mods = flags & MOD_MASK;

    // 1. A leader sequence is waiting for its key. Consume it whether or not
    //    it is bound - the daemon decides - so it never reaches the app.
    let armed = lock(&shared.armed_at).take();
    if let Some(armed_at) = armed {
        let timeout = Duration::from_millis(shared.leader_timeout_ms.load(Ordering::SeqCst));
        if armed_at.elapsed() <= timeout {
            let key = character_for(keycode as u16, flags & FLAG_SHIFT != 0);
            (shared.events.on_leader_key)(key);
            return std::ptr::null_mut();
        }
    }

    // 2. The leader or a direct hotkey. Copy what matched out of the lock
    //    before acting, so a callback can never re-enter it.
    let hit = lock(&shared.chords)
        .iter()
        .find(|r| r.chord.keycode == keycode && r.chord.flags == mods)
        .map(|r| (r.id, r.is_leader));
    if let Some((id, is_leader)) = hit {
        if is_leader {
            *lock(&shared.armed_at) = Some(Instant::now());
        } else {
            (shared.events.on_hotkey)(id);
        }
        return std::ptr::null_mut();
    }

    // 3. The paste chord while a mode is active: write, then let the very
    //    same Cmd+V through so the application pastes it (R21).
    if keycode == KEY_V && mods == FLAG_COMMAND && shared.intercept_active.load(Ordering::SeqCst) {
        (shared.events.on_paste_chord)();
    }
    event
}

// ------------------------------------------------------------------ hotkeys

pub struct MacTapHotkeys(Arc<Shared>);

impl HotkeyBackend for MacTapHotkeys {
    fn register(&mut self, trigger: &str, id: u32) -> Result<u32, String> {
        let chord = parse_chord(trigger)?;
        lock(&self.0.chords).push(Registered { chord, id, is_leader: false });
        ensure_started(&self.0);
        Ok(id)
    }

    fn register_leader(&mut self, trigger: &str, id: u32, timeout: Duration) -> Result<u32, String> {
        let chord = parse_chord(trigger)?;
        self.0.leader_timeout_ms.store(timeout.as_millis() as u64, Ordering::SeqCst);
        lock(&self.0.chords).push(Registered { chord, id, is_leader: true });
        ensure_started(&self.0);
        Ok(id)
    }

    fn unregister_all(&mut self) {
        lock(&self.0.chords).clear();
        *lock(&self.0.armed_at) = None;
    }

    fn unavailable_reason(&self) -> Option<String> {
        lock(&self.0.failure).clone()
    }

    fn name(&self) -> String {
        "macos-event-tap".into()
    }

    /// The tap reads the sequence key itself, so the server must not open a
    /// second tap for it.
    fn observes_leader_itself(&self) -> bool {
        true
    }
}

// ------------------------------------------------------------- interception

pub struct MacTapInterceptor(Arc<Shared>);

impl PasteInterceptor for MacTapInterceptor {
    fn set_active(&mut self, active: bool) {
        self.0.intercept_active.store(active, Ordering::SeqCst);
        if active {
            ensure_started(&self.0);
        }
    }

    fn name(&self) -> String {
        "macos-event-tap".into()
    }

    fn unavailable_reason(&self) -> Option<String> {
        lock(&self.0.failure).clone()
    }
}

// ------------------------------------------------------------------- chords

/// Virtual keycodes from Carbon's `Events.h`, with the characters the ANSI
/// layout prints on them. One table serves both directions: a chord names a
/// key, a leader sequence reads one.
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

/// Keys a chord can name that do not print a character.
const NAMED: &[(&str, u16)] = &[
    ("space", 0x31), ("enter", 0x24), ("return", 0x24), ("tab", 0x30),
    ("esc", 0x35), ("escape", 0x35), ("backspace", 0x33), ("delete", 0x75),
    ("left", 0x7B), ("right", 0x7C), ("down", 0x7D), ("up", 0x7E),
    ("home", 0x73), ("end", 0x77), ("pageup", 0x74), ("pagedown", 0x79),
    ("f1", 0x7A), ("f2", 0x78), ("f3", 0x63), ("f4", 0x76), ("f5", 0x60),
    ("f6", 0x61), ("f7", 0x62), ("f8", 0x64), ("f9", 0x65), ("f10", 0x6D),
    ("f11", 0x67), ("f12", 0x6F),
];

/// A keycode to the character a leader binding is written with.
///
/// ANSI layout. macOS exposes the layout-correct answer through
/// `CGEventKeyboardGetUnicodeString`, which `core-graphics` does not wrap; a
/// non-US layout resolves some keys by physical position.
pub fn character_for(keycode: u16, shifted: bool) -> Option<String> {
    KEYS.iter()
        .find(|(code, _, _)| *code == keycode)
        .map(|(_, plain, upper)| if shifted { *upper } else { *plain }.to_string())
}

/// `ctrl+alt+z` in any spelling to the flags and keycode the tap compares on.
pub fn parse_chord(trigger: &str) -> Result<Chord, String> {
    let normalized = copycat_protocol::normalize_trigger(trigger);
    let mut flags = 0u64;
    let mut key: Option<&str> = None;

    for token in normalized.split('+') {
        match token {
            "ctrl" => flags |= FLAG_CONTROL,
            "alt" => flags |= FLAG_OPTION,
            "shift" => flags |= FLAG_SHIFT,
            "super" | "cmdorctrl" => flags |= FLAG_COMMAND,
            "" => return Err(format!("`{trigger}` has an empty part")),
            other => {
                if key.replace(other).is_some() {
                    return Err(format!("`{trigger}` names more than one key"));
                }
            }
        }
    }

    let key = key.ok_or_else(|| format!("`{trigger}` names modifiers but no key"))?;
    let (keycode, needs_shift) = keycode_for(key)
        .ok_or_else(|| format!("`{key}` is not a key this keyboard has"))?;
    if needs_shift {
        flags |= FLAG_SHIFT;
    }
    Ok(Chord { flags, keycode: i64::from(keycode) })
}

/// A key name to its keycode, and whether reaching it needs shift.
fn keycode_for(name: &str) -> Option<(u16, bool)> {
    let lower = name.to_ascii_lowercase();
    if let Some((_, code)) = NAMED.iter().find(|(n, _)| *n == lower) {
        return Some((*code, false));
    }
    let mut chars = name.chars();
    let (c, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return None;
    }
    // A letter is the same key either case, and an uppercase letter in a
    // chord means "with shift"; punctuation is looked up on both levels.
    KEYS.iter().find_map(|(code, plain, upper)| {
        if *plain == c.to_ascii_lowercase() && c.is_ascii_alphabetic() {
            Some((*code, c.is_ascii_uppercase()))
        } else if *plain == c {
            Some((*code, false))
        } else if *upper == c {
            Some((*code, true))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_resolve_modifiers_and_keys() {
        let chord = parse_chord("ctrl+alt+z").unwrap();
        assert_eq!(chord.flags, FLAG_CONTROL | FLAG_OPTION);
        assert_eq!(chord.keycode, 0x06);

        let space = parse_chord("cmd+space").unwrap();
        assert_eq!(space.flags, FLAG_COMMAND);
        assert_eq!(space.keycode, 0x31);
    }

    #[test]
    fn every_spelling_of_command_and_option_works() {
        for spelling in ["cmd+v", "command+v", "super+v", "meta+v", "win+v"] {
            assert_eq!(parse_chord(spelling).unwrap().flags, FLAG_COMMAND, "{spelling}");
        }
        for spelling in ["alt+v", "option+v", "opt+v"] {
            assert_eq!(parse_chord(spelling).unwrap().flags, FLAG_OPTION, "{spelling}");
        }
    }

    #[test]
    fn an_uppercase_letter_in_a_chord_implies_shift() {
        assert_eq!(parse_chord("ctrl+S").unwrap().flags, FLAG_CONTROL | FLAG_SHIFT);
        assert_eq!(parse_chord("ctrl+shift+s").unwrap(), parse_chord("ctrl+S").unwrap());
    }

    #[test]
    fn a_chord_needs_exactly_one_key() {
        assert!(parse_chord("ctrl+alt").unwrap_err().contains("no key"));
        assert!(parse_chord("ctrl+a+b").unwrap_err().contains("more than one key"));
        assert!(parse_chord("ctrl+hyperkey").unwrap_err().contains("not a key"));
    }

    #[test]
    fn keycodes_read_back_to_the_characters_bindings_use() {
        assert_eq!(character_for(0x01, false).as_deref(), Some("s"));
        assert_eq!(character_for(0x01, true).as_deref(), Some("S"));
        assert_eq!(character_for(0x7E, false), None);
    }
}
