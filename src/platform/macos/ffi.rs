//! Raw bindings to the parts of CoreFoundation, CoreGraphics, the
//! Accessibility API (HIServices) and the Objective-C runtime that bspx uses.
//! Declared by hand to keep the dependency list short.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::{c_char, c_void};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFBooleanRef = *const c_void;
pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;

pub type AXUIElementRef = *const c_void;
pub type AXValueRef = *const c_void;
pub type AXObserverRef = *const c_void;
pub type AXObserverCallback =
    unsafe extern "C" fn(AXObserverRef, AXUIElementRef, CFStringRef, *mut c_void);
pub type AXError = i32;
pub const kAXErrorSuccess: AXError = 0;

pub type AXValueType = u32;
pub const kAXValueTypeCGPoint: AXValueType = 1;
pub const kAXValueTypeCGSize: AXValueType = 2;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
pub const kCFNumberSInt64Type: CFIndex = 4;
pub const kCFNumberFloat64Type: CFIndex = 6;

pub type CGWindowID = u32;
pub type CGDirectDisplayID = u32;
pub type CGWindowListOption = u32;
pub const kCGWindowListOptionAll: CGWindowListOption = 0;
pub const kCGWindowListOptionOnScreenOnly: CGWindowListOption = 1 << 0;
pub const kCGWindowListExcludeDesktopElements: CGWindowListOption = 1 << 4;
pub const kCGNullWindowID: CGWindowID = 0;

/// Current, other and user Spaces.
pub const kCGSAllSpacesMask: i32 = 0x7;

pub type CGEventSourceStateID = i32;
pub const kCGEventSourceStateCombinedSessionState: CGEventSourceStateID = 0;
pub type CGMouseButton = u32;
pub const kCGMouseButtonLeft: CGMouseButton = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// Stand-in for C structs only ever used by address.
#[repr(C)]
pub struct Opaque {
    _private: [u8; 0],
}

#[cfg_attr(target_os = "macos", link(name = "CoreFoundation", kind = "framework"))]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;

    pub fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: Boolean,
    ) -> CFStringRef;
    pub fn CFStringGetCString(
        s: CFStringRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> Boolean;

    pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;

    pub fn CFDictionaryCreate(
        alloc: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        key_callbacks: *const Opaque,
        value_callbacks: *const Opaque,
    ) -> CFDictionaryRef;
    pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;

    pub fn CFArrayCreate(
        alloc: *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        callbacks: *const Opaque,
    ) -> CFArrayRef;
    pub fn CFNumberCreate(alloc: *const c_void, ty: CFIndex, value: *const c_void) -> CFNumberRef;
    pub fn CFUUIDCreateString(alloc: *const c_void, uuid: CFTypeRef) -> CFStringRef;
    pub fn CFRunLoopGetCurrent() -> CFTypeRef;
    pub fn CFRunLoopAddSource(rl: CFTypeRef, source: CFTypeRef, mode: CFStringRef);
    pub fn CFRunLoopRemoveSource(rl: CFTypeRef, source: CFTypeRef, mode: CFStringRef);
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: Boolean)
    -> i32;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub fn CFNumberGetValue(number: CFNumberRef, ty: CFIndex, out: *mut c_void) -> Boolean;
    pub fn CFBooleanGetValue(boolean: CFBooleanRef) -> Boolean;
    pub fn CFPreferencesAppSynchronize(app: CFStringRef) -> Boolean;
    pub fn CFPreferencesCopyAppValue(key: CFStringRef, app: CFStringRef) -> CFTypeRef;

    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;
    pub static kCFTypeArrayCallBacks: Opaque;
    pub static kCFTypeDictionaryKeyCallBacks: Opaque;
    pub static kCFTypeDictionaryValueCallBacks: Opaque;
}

#[cfg_attr(
    target_os = "macos",
    link(name = "ApplicationServices", kind = "framework")
)]
unsafe extern "C" {
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    pub fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    pub fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    pub fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    pub fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut Boolean,
    ) -> AXError;
    pub fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    pub fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_seconds: f32)
    -> AXError;
    pub fn AXObserverCreate(
        pid: i32,
        callback: AXObserverCallback,
        out: *mut AXObserverRef,
    ) -> AXError;
    pub fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> AXError;
    pub fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> CFTypeRef;
    pub fn AXValueCreate(ty: AXValueType, value: *const c_void) -> AXValueRef;
    pub fn AXValueGetValue(value: AXValueRef, ty: AXValueType, out: *mut c_void) -> Boolean;

    /// Private, but stable for a decade and used by every macOS tiling WM:
    /// maps an AX window element to its CoreGraphics window number.
    pub fn _AXUIElementGetWindow(element: AXUIElementRef, out: *mut CGWindowID) -> AXError;

    pub static kAXTrustedCheckOptionPrompt: CFStringRef;

}

#[cfg_attr(target_os = "macos", link(name = "ColorSync", kind = "framework"))]
unsafe extern "C" {
    pub fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> CFTypeRef;
}

#[cfg_attr(target_os = "macos", link(name = "CoreGraphics", kind = "framework"))]
unsafe extern "C" {
    pub fn CGWindowListCopyWindowInfo(
        option: CGWindowListOption,
        relative_to: CGWindowID,
    ) -> CFArrayRef;
    pub fn CGRectMakeWithDictionaryRepresentation(dict: CFDictionaryRef, rect: *mut CGRect)
    -> bool;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGEventCreate(source: *const c_void) -> CFTypeRef;
    pub fn CGEventGetLocation(event: CFTypeRef) -> CGPoint;
    pub fn CGColorCreateSRGB(red: f64, green: f64, blue: f64, alpha: f64) -> CFTypeRef;
    pub fn CGColorRelease(color: CFTypeRef);
    pub fn CGEventSourceButtonState(state_id: CGEventSourceStateID, button: CGMouseButton) -> bool;
    pub fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: Boolean) -> i32;

    // Private SkyLight API, re-exported by CoreGraphics. Spaces have no
    // public API.
    pub fn CGSMainConnectionID() -> i32;
    pub fn CGSGetActiveSpace(cid: i32) -> u64;
    pub fn CGSManagedDisplayGetCurrentSpace(cid: i32, display_uuid: CFStringRef) -> u64;
    pub fn CGSCopySpacesForWindows(cid: i32, mask: i32, windows: CFArrayRef) -> CFArrayRef;

    pub static kCGWindowNumber: CFStringRef;
    pub static kCGWindowOwnerPID: CFStringRef;
    pub static kCGWindowOwnerName: CFStringRef;
    pub static kCGWindowLayer: CFStringRef;
    pub static kCGWindowBounds: CFStringRef;
    pub static kCGWindowAlpha: CFStringRef;
}

pub type Id = *mut c_void;
pub type Sel = *mut c_void;

// Linking AppKit makes NSScreen, NSApplication and NSWindow available to
// the Objective-C runtime; QuartzCore does the same for CATransaction.
#[cfg_attr(target_os = "macos", link(name = "AppKit", kind = "framework"))]
unsafe extern "C" {}

#[cfg_attr(target_os = "macos", link(name = "QuartzCore", kind = "framework"))]
unsafe extern "C" {}

#[cfg_attr(target_os = "macos", link(name = "Foundation", kind = "framework"))]
unsafe extern "C" {
    pub static NSDefaultRunLoopMode: Id;
}

#[cfg_attr(target_os = "macos", link(name = "objc"))]
unsafe extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Id;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    /// Called through a transmuted, correctly typed function pointer.
    pub fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    pub fn objc_msgSend_stret();
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// `dlsym`'s pseudo-handle that searches every loaded image.
pub const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

// libSystem, linked into every binary.
unsafe extern "C" {
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `CoreDockGetOrientationAndPinning` orientations.
pub const DOCK_ORIENTATION_BOTTOM: i32 = 2;
pub const DOCK_ORIENTATION_LEFT: i32 = 3;
pub const DOCK_ORIENTATION_RIGHT: i32 = 4;

/// Private Dock and SkyLight functions, looked up at run time so a macOS
/// release that drops one only loses what it was used for, not the whole
/// binary. macOS window managers commonly rely on these.
pub type CoreDockGetAutoHideEnabledFn = unsafe extern "C" fn() -> Boolean;
pub type CoreDockGetOrientationAndPinningFn =
    unsafe extern "C" fn(orientation: *mut i32, pinning: *mut i32);
pub type SLSGetDockRectWithReasonFn =
    unsafe extern "C" fn(cid: i32, rect: *mut CGRect, reason: *mut i32) -> i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessSerialNumber {
    pub high: u32,
    pub low: u32,
}

pub type GetProcessForPIDFn = unsafe extern "C" fn(pid: i32, psn: *mut ProcessSerialNumber) -> i32;
pub type SLPSSetFrontProcessWithOptionsFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, wid: u32, mode: u32) -> i32;
pub type SLPSPostEventRecordToFn =
    unsafe extern "C" fn(psn: *const ProcessSerialNumber, bytes: *const u8) -> i32;

/// `_SLPSSetFrontProcessWithOptions` mode: as if the user did it.
pub const CPS_USER_GENERATED: u32 = 0x200;

pub type SLSCopyManagedDisplaySpacesFn = unsafe extern "C" fn(cid: i32) -> CFArrayRef;
pub type SLSSpaceSetCompatIDFn = unsafe extern "C" fn(cid: i32, sid: u64, workspace: i32) -> i32;
pub type SLSSetWindowListWorkspaceFn =
    unsafe extern "C" fn(cid: i32, wids: *const u32, count: i32, workspace: i32) -> i32;
pub type SLSMoveWindowsToManagedSpaceFn =
    unsafe extern "C" fn(cid: i32, wids: CFArrayRef, sid: u64);
