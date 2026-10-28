//! Switching Spaces, and carrying a window along, through the macOS
//! "Switch to Desktop N" keyboard shortcuts (System Settings > Keyboard >
//! Keyboard Shortcuts > Mission Control). Nothing else moves windows
//! between Spaces or switches Spaces with SIP enabled on recent macOS.
//!
//! A window is carried by doing what a person would: hold it by its title
//! bar and press the shortcut.

use std::ffi::c_void;
use std::ptr;
use std::thread::sleep;
use std::time::Duration;

use super::ffi::*;
use super::{Cf, bool_value, cfstring, i64_value, string_value};
use crate::bsp::WinId;
use crate::geometry::Rect;

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
unsafe extern "C" {
    fn CGEventCreateMouseEvent(
        source: *const c_void,
        kind: u32,
        point: CGPoint,
        button: u32,
    ) -> CFTypeRef;
    fn CGEventCreateKeyboardEvent(source: *const c_void, keycode: u16, down: bool) -> CFTypeRef;
    fn CGEventSetFlags(event: CFTypeRef, flags: u64);
    fn CGEventPost(tap: u32, event: CFTypeRef);
}

#[cfg_attr(
    target_os = "macos",
    link(name = "ApplicationServices", kind = "framework")
)]
unsafe extern "C" {
    fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: f32,
        y: f32,
        element: *mut AXUIElementRef,
    ) -> AXError;
}

const HID_EVENT_TAP: u32 = 0;
const MOUSE_MOVED: u32 = 5;
const LEFT_MOUSE_DOWN: u32 = 1;
const LEFT_MOUSE_UP: u32 = 2;
const LEFT_MOUSE_DRAGGED: u32 = 6;
const LEFT_BUTTON: u32 = 0;

/// Desktop n's shortcut has this id in `com.apple.symbolichotkeys`.
const FIRST_DESKTOP_HOTKEY: usize = 117;

/// The key code and modifier flags of the "Switch to Desktop n" shortcut.
pub fn shortcut(n: usize) -> Result<(u16, u64), String> {
    let missing = || {
        format!(
            "the shortcut \"Switch to Desktop {n}\" is off; turn it on in System Settings > \
             Keyboard > Keyboard Shortcuts > Mission Control"
        )
    };
    unsafe {
        let app = cfstring("com.apple.symbolichotkeys");
        let key = cfstring("AppleSymbolicHotKeys");
        CFPreferencesAppSynchronize(app.ptr());
        let hotkeys =
            Cf::owned(CFPreferencesCopyAppValue(key.ptr(), app.ptr())).ok_or_else(missing)?;
        if CFGetTypeID(hotkeys.ptr()) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let id = cfstring(&(FIRST_DESKTOP_HOTKEY + n).to_string());
        let entry = CFDictionaryGetValue(hotkeys.ptr(), id.ptr());
        if entry.is_null() || CFGetTypeID(entry) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let enabled = CFDictionaryGetValue(entry, cfstring("enabled").ptr());
        let on = bool_value(enabled).or_else(|| i64_value(enabled).map(|v| v != 0));
        if on != Some(true) {
            return Err(missing());
        }
        let value = CFDictionaryGetValue(entry, cfstring("value").ptr());
        if value.is_null() || CFGetTypeID(value) != CFDictionaryGetTypeID() {
            return Err(missing());
        }
        let params = CFDictionaryGetValue(value, cfstring("parameters").ptr());
        if params.is_null()
            || CFGetTypeID(params) != CFArrayGetTypeID()
            || CFArrayGetCount(params) < 3
        {
            return Err(missing());
        }
        let keycode = i64_value(CFArrayGetValueAtIndex(params, 1)).ok_or_else(missing)?;
        let flags = i64_value(CFArrayGetValueAtIndex(params, 2)).ok_or_else(missing)?;
        Ok((keycode as u16, flags as u64))
    }
}

fn post_key(keycode: u16, flags: u64) {
    for down in [true, false] {
        unsafe {
            if let Some(e) = Cf::owned(CGEventCreateKeyboardEvent(ptr::null(), keycode, down)) {
                CGEventSetFlags(e.ptr(), flags);
                CGEventPost(HID_EVENT_TAP, e.ptr());
            }
        }
    }
}

fn post_mouse(kind: u32, x: f64, y: f64) {
    unsafe {
        if let Some(e) = Cf::owned(CGEventCreateMouseEvent(
            ptr::null(),
            kind,
            CGPoint { x, y },
            LEFT_BUTTON,
        )) {
            CGEventSetFlags(e.ptr(), 0);
            CGEventPost(HID_EVENT_TAP, e.ptr());
        }
    }
}

/// Switches to Space n with its keyboard shortcut.
pub fn switch(n: usize) -> Result<(), String> {
    let (keycode, flags) = shortcut(n)?;
    post_key(keycode, flags);
    Ok(())
}

/// A point on the window's title bar that drags the window when held:
/// empty title bar, toolbar background or the title, never a button, a
/// text field or a tab.
fn grab_point(system: AXUIElementRef, pid: i32, frame: Rect) -> Option<(f64, f64)> {
    let role_key = cfstring("AXRole");
    let w = f64::from(frame.w);
    let xs = [
        76.0,
        w * 0.5,
        w * 0.35,
        w * 0.65,
        w - 70.0,
        110.0,
        150.0,
        w * 0.2,
        w * 0.8,
    ];
    let ys = [10.0, 20.0, 6.0];
    for dy in ys {
        for dx in xs {
            if dx < 4.0 || dx > w - 4.0 {
                continue;
            }
            let (x, y) = (f64::from(frame.x) + dx, f64::from(frame.y) + dy);
            unsafe {
                let mut element: AXUIElementRef = ptr::null();
                if AXUIElementCopyElementAtPosition(system, x as f32, y as f32, &raw mut element)
                    != 0
                {
                    continue;
                }
                let Some(element) = Cf::owned(element) else {
                    continue;
                };
                let mut owner = 0;
                if AXUIElementGetPid(element.ptr(), &raw mut owner) != 0 || owner != pid {
                    continue;
                }
                let mut role: CFTypeRef = ptr::null();
                AXUIElementCopyAttributeValue(element.ptr(), role_key.ptr(), &raw mut role);
                let role = Cf::owned(role).and_then(|r| string_value(r.ptr()));
                if matches!(
                    role.as_deref(),
                    Some("AXWindow" | "AXToolbar" | "AXGroup" | "AXStaticText" | "AXUnknown")
                ) {
                    return Some((x, y));
                }
            }
        }
    }
    None
}

/// Carries window `frame` of app `pid` to Space n: holds it by the title
/// bar and switches Spaces, as a person would. Ends on Space n. `moved`
/// tells whether the window has arrived.
pub fn carry(
    system: AXUIElementRef,
    pid: i32,
    wid: WinId,
    frame: Rect,
    n: usize,
    mut moved: impl FnMut() -> bool,
) -> Result<(), String> {
    let (keycode, flags) = shortcut(n)?;
    let (x, y) = grab_point(system, pid, frame)
        .ok_or_else(|| format!("window {wid} has no title bar to hold it by"))?;
    let back =
        unsafe { Cf::owned(CGEventCreate(ptr::null())).map(|e| CGEventGetLocation(e.ptr())) };

    post_mouse(MOUSE_MOVED, x, y);
    post_mouse(LEFT_MOUSE_DOWN, x, y);
    sleep(Duration::from_millis(60));
    post_mouse(LEFT_MOUSE_DRAGGED, x + 1.0, y);
    sleep(Duration::from_millis(60));
    post_key(keycode, flags);
    // The switch animation, with the window held.
    sleep(Duration::from_millis(450));
    post_mouse(LEFT_MOUSE_UP, x + 1.0, y);
    if let Some(p) = back {
        unsafe {
            CGWarpMouseCursorPosition(p);
            CGAssociateMouseAndMouseCursorPosition(1);
        }
    }
    let mut arrived = false;
    for _ in 0..20 {
        if moved() {
            arrived = true;
            break;
        }
        sleep(Duration::from_millis(50));
    }
    if arrived {
        Ok(())
    } else {
        Err(format!("window {wid} did not come along to Space {n}"))
    }
}
