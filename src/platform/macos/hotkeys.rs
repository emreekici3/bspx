//! Keyboard shortcuts on macOS: a key-down event tap on a thread of its own
//! (like the mouse tap: macOS holds each key press until the tap answers),
//! and the keyboard layout, to know which key types which character.
//!
//! The tap only looks the press up. A `bspx ...` command is queued for the
//! daemon's loop; anything else goes to the shell from another thread.

use std::collections::HashMap;
use std::ffi::c_void;
use std::process::Command as Process;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering::Relaxed};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use super::ffi::{
    AXIsProcessTrustedWithOptions, CFIndex, CFRelease, CFRunLoopAddSource, CFRunLoopGetCurrent,
    CFRunLoopRunInMode, CFStringRef, CFTypeRef, kCFRunLoopDefaultMode,
};
use super::string_value;
use crate::hotkeys::{Action, Held, Hotkeys};

type CGEventTapCallBack =
    unsafe extern "C" fn(*mut c_void, u32, CFTypeRef, *mut c_void) -> CFTypeRef;

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFTypeRef;
    fn CGEventTapEnable(tap: CFTypeRef, enable: bool);
    fn CGEventGetFlags(event: CFTypeRef) -> u64;
    fn CGEventGetIntegerValueField(event: CFTypeRef, field: u32) -> i64;
}

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFTypeRef,
        order: CFIndex,
    ) -> CFTypeRef;
    fn CFMachPortInvalidate(port: CFTypeRef);
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFStringCreateWithCharacters(
        allocator: *const c_void,
        chars: *const u16,
        len: CFIndex,
    ) -> CFStringRef;
    static kCFRunLoopCommonModes: CFStringRef;
}

#[cfg_attr(target_os = "macos", link(name = "Carbon", kind = "framework"))]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> CFTypeRef;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> CFTypeRef;
    fn TISGetInputSourceProperty(source: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    static kTISPropertyInputSourceID: CFStringRef;
}

const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT_EVENT_TAP: u32 = 0;
const TAP_OPTION_DEFAULT: u32 = 0;
const KEY_DOWN: u32 = 10;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
/// `kCGKeyboardEventKeycode`.
const KEYCODE_FIELD: u32 = 9;

// `CGEventFlags`: the device-independent modifier bits, and the left and
// right keys (`NX_DEVICE*KEYMASK`).
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CTRL: u64 = 0x0004_0000;
const FLAG_ALT: u64 = 0x0008_0000;
const FLAG_CMD: u64 = 0x0010_0000;
const FLAG_FN: u64 = 0x0080_0000;
const DEV_LCTRL: u64 = 0x0000_0001;
const DEV_LSHIFT: u64 = 0x0000_0002;
const DEV_RSHIFT: u64 = 0x0000_0004;
const DEV_LCMD: u64 = 0x0000_0008;
const DEV_RCMD: u64 = 0x0000_0010;
const DEV_LALT: u64 = 0x0000_0020;
const DEV_RALT: u64 = 0x0000_0040;
const DEV_RCTRL: u64 = 0x0000_2000;

/// How often the tap thread checks that bspx may still use Accessibility,
/// in seconds.
const TRUST_CHECK_SECS: f64 = 0.25;
/// How often the keyboard layout is checked for a change.
const LAYOUT_CHECK: Duration = Duration::from_secs(1);

/// The shortcuts the tap matches against.
static HOTKEYS: Mutex<Option<Hotkeys>> = Mutex::new(None);
/// `bspx ...` command lines for the daemon.
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SHELL: OnceLock<Mutex<Sender<String>>> = OnceLock::new();
static TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static START: Once = Once::new();

/// Which keys of a modifier pair are down. An event that sets the pair's
/// flag without saying which key (a synthetic one) counts as the left.
fn pair(flags: u64, any: u64, left: u64, right: u64) -> (bool, bool) {
    if flags & any == 0 {
        return (false, false);
    }
    let (l, r) = (flags & left != 0, flags & right != 0);
    if l || r { (l, r) } else { (true, false) }
}

fn held(flags: u64) -> Held {
    Held {
        alt: pair(flags, FLAG_ALT, DEV_LALT, DEV_RALT),
        shift: pair(flags, FLAG_SHIFT, DEV_LSHIFT, DEV_RSHIFT),
        cmd: pair(flags, FLAG_CMD, DEV_LCMD, DEV_RCMD),
        ctrl: pair(flags, FLAG_CTRL, DEV_LCTRL, DEV_RCTRL),
        fun: flags & FLAG_FN != 0,
    }
}

/// The current keyboard layout's id, and which key types each character
/// with no modifiers held. Must run on the main thread.
fn keyboard_layout() -> Option<(String, HashMap<char, u16>)> {
    unsafe {
        // A layout that types Latin letters, even while an input method
        // for another script is selected.
        let mut source = TISCopyCurrentKeyboardLayoutInputSource();
        let mut data = if source.is_null() {
            ptr::null()
        } else {
            TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData)
        };
        if data.is_null() {
            if !source.is_null() {
                CFRelease(source);
            }
            source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
            if source.is_null() {
                return None;
            }
            data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        }
        let layout = if data.is_null() {
            ptr::null()
        } else {
            CFDataGetBytePtr(data)
        };
        if layout.is_null() {
            CFRelease(source);
            return None;
        }
        let id = string_value(TISGetInputSourceProperty(source, kTISPropertyInputSourceID))
            .unwrap_or_default();
        let kind = u32::from(LMGetKbdType());
        let mut map = HashMap::new();
        // The lower key code wins, so the main row beats the keypad.
        for code in 0u16..128 {
            let mut chars = [0u16; 4];
            let mut len = 0usize;
            let mut dead = 0u32;
            // kUCKeyActionDown, no modifiers, kUCKeyTranslateNoDeadKeysMask.
            let status = UCKeyTranslate(
                layout.cast(),
                code,
                0,
                0,
                kind,
                1,
                &mut dead,
                chars.len(),
                &mut len,
                chars.as_mut_ptr(),
            );
            if status != 0 || len != 1 {
                continue;
            }
            let s = CFStringCreateWithCharacters(ptr::null(), chars.as_ptr(), 1);
            if s.is_null() {
                continue;
            }
            if let Some(c) = string_value(s).and_then(|t| t.chars().next())
                && !c.is_control()
                && !c.is_whitespace()
            {
                for lower in c.to_lowercase() {
                    map.entry(lower).or_insert(code);
                }
            }
            CFRelease(s);
        }
        CFRelease(source);
        Some((id, map))
    }
}

/// Keeps the shortcuts in step with bspxrc and the keyboard layout; lives
/// on the daemon's thread.
#[derive(Default)]
pub struct HotkeyManager {
    text: String,
    layout_id: String,
    layout: HashMap<char, u16>,
    last_check: Option<Instant>,
}

impl HotkeyManager {
    /// Sets the shortcuts, one per line.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        if self.layout_id.is_empty() {
            self.read_layout();
        }
        self.build();
    }

    /// Rebuilds the shortcuts when the keyboard layout changed. Called
    /// every frame; looks once a second.
    pub fn poll(&mut self) {
        if self.text.is_empty() || self.last_check.is_some_and(|t| t.elapsed() < LAYOUT_CHECK) {
            return;
        }
        self.last_check = Some(Instant::now());
        if self.read_layout() {
            debug!("keyboard layout changed, rebuilding shortcuts");
            self.build();
        }
    }

    /// Whether the layout is a different one than before.
    fn read_layout(&mut self) -> bool {
        match keyboard_layout() {
            Some((id, map)) if id != self.layout_id => {
                self.layout_id = id;
                self.layout = map;
                true
            }
            _ => false,
        }
    }

    fn build(&mut self) {
        let (hotkeys, problems) = Hotkeys::build(&self.text, &self.layout);
        for p in &problems {
            warn!("shortcut skipped: {p}");
        }
        debug!("{} shortcuts", hotkeys.count());
        let any = hotkeys.count() > 0;
        if let Ok(mut h) = HOTKEYS.lock() {
            *h = Some(hotkeys);
        }
        if any {
            START.call_once(start);
        }
    }

    /// The `bspx ...` command lines pressed since the last call.
    pub fn take_lines(&mut self) -> Vec<String> {
        LINES
            .lock()
            .map(|mut l| std::mem::take(&mut *l))
            .unwrap_or_default()
    }
}

/// `PATH` with bspx's own directory first, so commands find `bspx`.
fn path_with_bspx() -> Option<std::ffi::OsString> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![dir];
    dirs.extend(std::env::split_paths(&path_var));
    std::env::join_paths(dirs).ok()
}

/// Runs shell commands from a thread of its own, so the tap never waits on
/// starting a process.
fn shell() -> &'static Mutex<Sender<String>> {
    SHELL.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        let spawned = std::thread::Builder::new()
            .name("bspx-shortcut-shell".into())
            .spawn(move || {
                let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let path = path_with_bspx();
                for command in rx {
                    let mut cmd = Process::new(&sh);
                    cmd.arg("-c").arg(&command);
                    if let Some(p) = &path {
                        cmd.env("PATH", p);
                    }
                    match cmd.spawn() {
                        // Wait for it elsewhere, so it does not linger as a zombie.
                        Ok(mut child) => {
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            });
                        }
                        Err(e) => warn!("shortcut command '{command}' failed: {e}"),
                    }
                }
            });
        if let Err(e) = spawned {
            warn!("shortcuts cannot run shell commands: {e}");
        }
        Mutex::new(tx)
    })
}

fn run(action: Action) {
    match action {
        Action::Bspx(line) => {
            debug!("shortcut: bspx {line}");
            if let Ok(mut l) = LINES.lock() {
                l.push(line);
            }
        }
        Action::Shell(command) => {
            debug!("shortcut: {command}");
            if let Ok(tx) = shell().lock() {
                let _ = tx.send(command);
            }
        }
    }
}

fn start() {
    let _ = shell();
    let spawned = std::thread::Builder::new()
        .name("bspx-keyboard".into())
        .spawn(|| unsafe {
            let tap = CGEventTapCreate(
                SESSION_EVENT_TAP,
                HEAD_INSERT_EVENT_TAP,
                TAP_OPTION_DEFAULT,
                1u64 << KEY_DOWN,
                callback,
                ptr::null_mut(),
            );
            if tap.is_null() {
                warn!("shortcuts are unavailable: macOS refused the event tap");
                return;
            }
            TAP.store(tap.cast_mut(), Relaxed);
            let source = CFMachPortCreateRunLoopSource(ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            info!("keyboard shortcuts enabled");
            // Without Accessibility, macOS keeps routing key presses through
            // a tap that can no longer answer, and the keyboard freezes: get
            // out of the way at once.
            while AXIsProcessTrustedWithOptions(ptr::null()) != 0 {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, TRUST_CHECK_SECS, 0);
            }
            CGEventTapEnable(tap, false);
            TAP.store(ptr::null_mut(), Relaxed);
            CFMachPortInvalidate(tap);
            warn!("accessibility permission was removed; restarting");
            std::process::exit(1);
        });
    if let Err(e) = spawned {
        warn!("shortcuts are unavailable: {e}");
    }
}

unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: CFTypeRef,
    _user_info: *mut c_void,
) -> CFTypeRef {
    match kind {
        TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
            let tap = TAP.load(Relaxed);
            if !tap.is_null() {
                unsafe { CGEventTapEnable(tap, true) };
            }
            return event;
        }
        KEY_DOWN => {}
        _ => return event,
    }
    let (code, flags) = unsafe {
        (
            CGEventGetIntegerValueField(event, KEYCODE_FIELD) as u16,
            CGEventGetFlags(event),
        )
    };
    let found = HOTKEYS
        .lock()
        .ok()
        .and_then(|h| h.as_ref().and_then(|h| h.press(code, &held(flags))));
    match found {
        Some((action, swallow)) => {
            run(action);
            if swallow { ptr::null() } else { event }
        }
        None => event,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags() {
        let h = held(FLAG_ALT | DEV_LALT | FLAG_SHIFT | DEV_RSHIFT);
        assert_eq!(h.alt, (true, false));
        assert_eq!(h.shift, (false, true));
        assert_eq!(h.cmd, (false, false));
        assert!(!h.fun);
        // No side given: the left one.
        assert_eq!(held(FLAG_CMD).cmd, (true, false));
        assert!(held(FLAG_FN).fun);
    }
}
