//! Mouse gestures: an event tap that catches clicks made with the gesture
//! modifier held, so a window can be moved or resized from anywhere on it.
//!
//! The tap runs on a thread of its own. macOS holds back every mouse event
//! the tap listens to until the tap answers, so it must not wait for the
//! window manager's frame loop. The tap only records which button is held;
//! the manager reads that every frame.

use std::ffi::c_void;
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, Ordering::Relaxed};

use log::{info, warn};

use super::ffi::{
    AXIsProcessTrustedWithOptions, CFIndex, CFRunLoopAddSource, CFRunLoopGetCurrent,
    CFRunLoopRunInMode, CFStringRef, CFTypeRef, CGPoint, kCFRunLoopDefaultMode,
};
use crate::platform::{Modifier, MouseButton};

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
    fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
}

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFTypeRef,
        order: CFIndex,
    ) -> CFTypeRef;
    fn CFMachPortInvalidate(port: CFTypeRef);
    static kCFRunLoopCommonModes: CFStringRef;
}

const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT_EVENT_TAP: u32 = 0;
const TAP_OPTION_DEFAULT: u32 = 0;

const LEFT_MOUSE_DOWN: u32 = 1;
const LEFT_MOUSE_UP: u32 = 2;
const RIGHT_MOUSE_DOWN: u32 = 3;
const RIGHT_MOUSE_UP: u32 = 4;
const LEFT_MOUSE_DRAGGED: u32 = 6;
const RIGHT_MOUSE_DRAGGED: u32 = 7;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

/// How often the tap thread checks that bspx may still use Accessibility,
/// in seconds.
const TRUST_CHECK_SECS: f64 = 0.25;

/// The modifier's flag (`CGEventFlags`), or 0 for no gestures.
static MASK: AtomicU64 = AtomicU64::new(0);
/// The button of the gesture in progress: 0 none, 1 left, 2 right.
static ACTIVE: AtomicU8 = AtomicU8::new(0);
/// The button of the last gesture started, until the manager has seen it,
/// so a click quicker than a frame is not missed.
static LATCHED: AtomicU8 = AtomicU8::new(0);
/// Where the pointer was at the last event of the gesture, as two `f32`s
/// packed into one word. The events are swallowed, so the system may not
/// report the pointer moving while they are.
static LOCATION: AtomicU64 = AtomicU64::new(0);
static TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static START: Once = Once::new();

fn flag(modifier: Modifier) -> u64 {
    match modifier {
        Modifier::Shift => 0x0002_0000,
        Modifier::Ctrl => 0x0004_0000,
        Modifier::Alt => 0x0008_0000,
        Modifier::Cmd => 0x0010_0000,
        Modifier::Fn => 0x0080_0000,
    }
}

fn button(code: u8) -> Option<MouseButton> {
    match code {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Right),
        _ => None,
    }
}

/// Sets the gesture modifier; the tap is started the first time there is one.
pub fn set_modifier(modifier: Option<Modifier>) {
    MASK.store(modifier.map_or(0, flag), Relaxed);
    if modifier.is_some() {
        START.call_once(start);
    }
}

/// The button of the gesture in progress, or of one that ended since the
/// last call.
pub fn gesture() -> Option<MouseButton> {
    let latched = LATCHED.swap(0, Relaxed);
    button(ACTIVE.load(Relaxed)).or(button(latched))
}

/// The pointer during a gesture, from the gesture's own events.
pub fn location() -> Option<(f32, f32)> {
    if ACTIVE.load(Relaxed) == 0 {
        return None;
    }
    let bits = LOCATION.load(Relaxed);
    Some((
        f32::from_bits((bits >> 32) as u32),
        f32::from_bits(bits as u32),
    ))
}

fn store_location(event: CFTypeRef) {
    let p = unsafe { CGEventGetLocation(event) };
    let bits = (u64::from((p.x as f32).to_bits()) << 32) | u64::from((p.y as f32).to_bits());
    LOCATION.store(bits, Relaxed);
}

fn start() {
    let spawned = std::thread::Builder::new()
        .name("bspx-mouse".into())
        .spawn(|| unsafe {
            let events = [
                LEFT_MOUSE_DOWN,
                LEFT_MOUSE_UP,
                RIGHT_MOUSE_DOWN,
                RIGHT_MOUSE_UP,
                LEFT_MOUSE_DRAGGED,
                RIGHT_MOUSE_DRAGGED,
            ]
            .iter()
            .fold(0u64, |mask, e| mask | (1 << e));
            if AXIsProcessTrustedWithOptions(ptr::null()) == 0 {
                warn!("mouse gestures are unavailable: no accessibility permission");
                return;
            }
            let tap = CGEventTapCreate(
                SESSION_EVENT_TAP,
                HEAD_INSERT_EVENT_TAP,
                TAP_OPTION_DEFAULT,
                events,
                callback,
                ptr::null_mut(),
            );
            if tap.is_null() {
                warn!("mouse gestures are unavailable: macOS refused the event tap");
                return;
            }
            TAP.store(tap.cast_mut(), Relaxed);
            let source = CFMachPortCreateRunLoopSource(ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            info!("mouse gestures enabled");
            // Once Accessibility is taken away, macOS keeps routing mouse
            // events through a tap that can no longer answer, and the mouse
            // freezes. So keep checking, and get out of the way at once.
            while AXIsProcessTrustedWithOptions(ptr::null()) != 0 {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, TRUST_CHECK_SECS, 0);
            }
            CGEventTapEnable(tap, false);
            TAP.store(ptr::null_mut(), Relaxed);
            CFMachPortInvalidate(tap);
            warn!("accessibility permission was removed; restarting");
            // launchd starts bspx again, which then waits for the permission.
            std::process::exit(1);
        });
    if let Err(e) = spawned {
        warn!("mouse gestures are unavailable: {e}");
    }
}

/// Swallows the clicks and drags of a gesture; lets everything else through.
unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: CFTypeRef,
    _user_info: *mut c_void,
) -> CFTypeRef {
    let code = match kind {
        TAP_DISABLED_BY_TIMEOUT | TAP_DISABLED_BY_USER_INPUT => {
            // Events went by unseen, the button-up perhaps among them.
            ACTIVE.store(0, Relaxed);
            let tap = TAP.load(Relaxed);
            if !tap.is_null() {
                unsafe { CGEventTapEnable(tap, true) };
            }
            return event;
        }
        LEFT_MOUSE_DOWN | LEFT_MOUSE_UP | LEFT_MOUSE_DRAGGED => 1,
        RIGHT_MOUSE_DOWN | RIGHT_MOUSE_UP | RIGHT_MOUSE_DRAGGED => 2,
        _ => return event,
    };
    let active = ACTIVE.load(Relaxed);
    match kind {
        LEFT_MOUSE_DOWN | RIGHT_MOUSE_DOWN => {
            let mask = MASK.load(Relaxed);
            if mask != 0 && unsafe { CGEventGetFlags(event) } & mask == mask {
                store_location(event);
                ACTIVE.store(code, Relaxed);
                LATCHED.store(code, Relaxed);
                return ptr::null();
            }
            if active == code {
                // Pressed again without the modifier: the button-up of the
                // gesture was missed.
                ACTIVE.store(0, Relaxed);
            }
        }
        LEFT_MOUSE_UP | RIGHT_MOUSE_UP if active == code => {
            ACTIVE.store(0, Relaxed);
            return ptr::null();
        }
        _ if active == code => {
            store_location(event);
            return ptr::null();
        }
        _ => {}
    }
    event
}
