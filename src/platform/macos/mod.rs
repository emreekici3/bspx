//! macOS backend: CoreGraphics to see which windows are on screen, the
//! Accessibility API to move, resize and focus them.

mod desktop;
mod ffi;
mod hotkeys;
mod overlay;
mod tap;

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_void};
use std::ptr;

use log::{debug, info, warn};

use self::ffi::*;
use self::overlay::OverlayWindow;
use super::{Backend, Modifier, MouseButton, Overlay, WindowEvent, WindowInfo};
use crate::bsp::WinId;
use crate::geometry::{Dock, DockEdge, Rect, usable_area};

/// Owned CoreFoundation reference, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    /// Takes ownership of a +1 reference ("Create"/"Copy" functions).
    fn owned(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then(|| Self(ptr))
    }

    /// Retains a borrowed reference ("Get" functions).
    fn retained(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then(|| Self(unsafe { CFRetain(ptr) }))
    }

    fn ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn cfstring(s: &str) -> Cf {
    let ptr = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            s.as_ptr(),
            s.len() as CFIndex,
            kCFStringEncodingUTF8,
            0,
        )
    };
    Cf::owned(ptr).expect("CFStringCreateWithBytes failed")
}

fn string_value(v: CFTypeRef) -> Option<String> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = [0 as c_char; 512];
        if CFStringGetCString(
            v,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            kCFStringEncodingUTF8,
        ) == 0
        {
            return None;
        }
        Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
    }
}

fn i64_value(v: CFTypeRef) -> Option<i64> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        (CFNumberGetValue(v, kCFNumberSInt64Type, (&raw mut out).cast()) != 0).then_some(out)
    }
}

fn f64_value(v: CFTypeRef) -> Option<f64> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: f64 = 0.0;
        (CFNumberGetValue(v, kCFNumberFloat64Type, (&raw mut out).cast()) != 0).then_some(out)
    }
}

fn bool_value(v: CFTypeRef) -> Option<bool> {
    unsafe {
        if v.is_null() || CFGetTypeID(v) != CFBooleanGetTypeID() {
            return None;
        }
        Some(CFBooleanGetValue(v) != 0)
    }
}

/// Attribute and action names, created once.
struct Names {
    windows: Cf,
    position: Cf,
    size: Cf,
    subrole: Cf,
    standard_window: &'static str,
    fullscreen: Cf,
    focused_application: Cf,
    focused_window: Cf,
    main: Cf,
    frontmost: Cf,
    raise: Cf,
    enhanced_ui: Cf,
    destroyed: Cf,
    miniaturized: Cf,
    deminiaturized: Cf,
    app_hidden: Cf,
    app_shown: Cf,
}

impl Names {
    fn new() -> Self {
        Self {
            windows: cfstring("AXWindows"),
            position: cfstring("AXPosition"),
            size: cfstring("AXSize"),
            subrole: cfstring("AXSubrole"),
            standard_window: "AXStandardWindow",
            fullscreen: cfstring("AXFullScreen"),
            focused_application: cfstring("AXFocusedApplication"),
            focused_window: cfstring("AXFocusedWindow"),
            main: cfstring("AXMain"),
            frontmost: cfstring("AXFrontmost"),
            raise: cfstring("AXRaise"),
            enhanced_ui: cfstring("AXEnhancedUserInterface"),
            destroyed: cfstring("AXUIElementDestroyed"),
            miniaturized: cfstring("AXWindowMiniaturized"),
            deminiaturized: cfstring("AXWindowDeminiaturized"),
            app_hidden: cfstring("AXApplicationHidden"),
            app_shown: cfstring("AXApplicationShown"),
        }
    }
}

fn copy_attr(element: AXUIElementRef, attr: &Cf) -> Option<Cf> {
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(element, attr.ptr(), &mut value) };
    if err == kAXErrorSuccess {
        Cf::owned(value)
    } else {
        None
    }
}

fn set_attr(element: AXUIElementRef, attr: &Cf, value: CFTypeRef) -> bool {
    unsafe { AXUIElementSetAttributeValue(element, attr.ptr(), value) == kAXErrorSuccess }
}

fn is_settable(element: AXUIElementRef, attr: &Cf) -> bool {
    let mut settable: Boolean = 0;
    let err = unsafe { AXUIElementIsAttributeSettable(element, attr.ptr(), &mut settable) };
    err == kAXErrorSuccess && settable != 0
}

fn window_id(element: AXUIElementRef) -> Option<WinId> {
    let mut id: CGWindowID = 0;
    let err = unsafe { _AXUIElementGetWindow(element, &mut id) };
    (err == kAXErrorSuccess && id != 0).then_some(id)
}

/// Returns whether bspx may use the Accessibility API, asking the user for
/// permission (system dialog) when `prompt` is set.
pub fn ensure_accessibility(prompt: bool) -> bool {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt;
        let value = if prompt {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        };
        let options = CFDictionaryCreate(
            ptr::null(),
            &raw const key,
            &raw const value,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        let options = Cf::owned(options);
        AXIsProcessTrustedWithOptions(options.as_ref().map_or(ptr::null(), Cf::ptr)) != 0
    }
}

/// One on-screen window as CoreGraphics reports it.
struct CgWindow {
    id: WinId,
    pid: i32,
    app: String,
    frame: Rect,
}

/// Normal application windows, other than bspx's own. `on_screen_only`
/// limits them to the active Space; otherwise windows on every Space,
/// minimised and hidden ones included, are listed.
fn cg_windows(on_screen_only: bool) -> Vec<CgWindow> {
    let mut out = Vec::new();
    let options = if on_screen_only {
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    } else {
        kCGWindowListOptionAll
    };
    let own_pid = std::process::id() as i64;
    unsafe {
        let list = CGWindowListCopyWindowInfo(options, kCGNullWindowID);
        let Some(list) = Cf::owned(list) else {
            return out;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            // Layer 0 holds normal application windows; menus, the Dock,
            // overlays and the menu bar live on other layers.
            if i64_value(get(kCGWindowLayer)) != Some(0) {
                continue;
            }
            let (Some(id), Some(pid)) = (
                i64_value(get(kCGWindowNumber)),
                i64_value(get(kCGWindowOwnerPID)),
            ) else {
                continue;
            };
            if pid == own_pid {
                continue; // the drag highlight
            }
            let mut bounds = CGRect::default();
            let bounds_dict = get(kCGWindowBounds);
            if bounds_dict.is_null()
                || !CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut bounds)
            {
                continue;
            }
            out.push(CgWindow {
                id: id as WinId,
                pid: pid as i32,
                app: string_value(get(kCGWindowOwnerName)).unwrap_or_default(),
                frame: cg_to_rect(bounds),
            });
        }
    }
    out
}

/// Whether app `pid` shows a window above the normal ones, other than in
/// the menu bar and the Dock: Spotlight's search field, a launcher, a panel.
/// Spotlight counts whichever app has the keyboard: its panel need not
/// make it the focused app.
fn shows_panel(pid: Option<i32>) -> bool {
    unsafe {
        let Some(list) = Cf::owned(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )) else {
            return false;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            let owner = i64_value(get(kCGWindowOwnerPID));
            let ours = pid.is_some_and(|p| owner == Some(i64::from(p)))
                || string_value(get(kCGWindowOwnerName)).as_deref() == Some("Spotlight");
            if !ours || f64_value(get(kCGWindowAlpha)).is_some_and(|a| a <= 0.0) {
                continue;
            }
            // 20: the Dock; 24, 25: the menu bar and its status items, which
            // any app may own while it is idle.
            if let Some(layer) = i64_value(get(kCGWindowLayer))
                && layer > 0
                && !matches!(layer, 20 | 24 | 25)
            {
                return true;
            }
        }
    }
    false
}

/// Whether an open menu or popover contains a point.
fn covered_at(x: f32, y: f32) -> bool {
    let own_pid = std::process::id() as i64;
    unsafe {
        let Some(list) = Cf::owned(CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )) else {
            return false;
        };
        for i in 0..CFArrayGetCount(list.ptr()) {
            let dict = CFArrayGetValueAtIndex(list.ptr(), i);
            if dict.is_null() || CFGetTypeID(dict) != CFDictionaryGetTypeID() {
                continue;
            }
            let get = |key: CFStringRef| CFDictionaryGetValue(dict, key);
            // Only the pop-up menu layers (menus, popovers): focusing a
            // window would close them. The Dock and the menu bar stay in
            // front of a raised window anyway, and other layers hold
            // overlays of all kinds.
            let covering = |l: i64| matches!(l, 100..=103);
            if !i64_value(get(kCGWindowLayer)).is_some_and(covering)
                || f64_value(get(kCGWindowAlpha)).is_some_and(|a| a <= 0.0)
                || i64_value(get(kCGWindowOwnerPID)) == Some(own_pid)
            {
                continue;
            }
            let mut bounds = CGRect::default();
            let bounds_dict = get(kCGWindowBounds);
            if !bounds_dict.is_null()
                && CGRectMakeWithDictionaryRepresentation(bounds_dict, &mut bounds)
                && cg_to_rect(bounds).contains_point(x, y)
            {
                return true;
            }
        }
    }
    false
}

fn cg_to_rect(r: CGRect) -> Rect {
    Rect::new(
        r.origin.x.round() as i32,
        r.origin.y.round() as i32,
        r.size.width.round() as i32,
        r.size.height.round() as i32,
    )
}

/// The primary display's frame and its visible frame (menu bar and Dock
/// excluded), both in CoreGraphics coordinates (top-left origin).
fn primary_screen_area() -> Option<Rect> {
    type MsgSendId = unsafe extern "C" fn(Id, Sel) -> Id;
    type MsgSendIndex = unsafe extern "C" fn(Id, Sel, usize) -> Id;
    type MsgSendUsize = unsafe extern "C" fn(Id, Sel) -> usize;

    unsafe fn rect_of(obj: Id, sel: Sel) -> CGRect {
        #[cfg(target_arch = "x86_64")]
        {
            type Stret = unsafe extern "C" fn(*mut CGRect, Id, Sel);
            let f: Stret =
                unsafe { std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn()) };
            let mut out = CGRect::default();
            unsafe { f(&mut out, obj, sel) };
            out
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            type Ret = unsafe extern "C" fn(Id, Sel) -> CGRect;
            let f: Ret = unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
            unsafe { f(obj, sel) }
        }
    }

    unsafe {
        let send_id: MsgSendId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_index: MsgSendIndex = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_usize: MsgSendUsize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let pool = objc_autoreleasePoolPush();
        let result = (|| {
            let class = objc_getClass(c"NSScreen".as_ptr());
            if class.is_null() {
                return None;
            }
            let screens = send_id(class, sel_registerName(c"screens".as_ptr()));
            if screens.is_null() || send_usize(screens, sel_registerName(c"count".as_ptr())) == 0 {
                return None;
            }
            // Index 0 is the screen with the menu bar, whose bottom-left
            // corner is the origin of Cocoa's global coordinates.
            let primary = send_index(screens, sel_registerName(c"objectAtIndex:".as_ptr()), 0);
            let frame = rect_of(primary, sel_registerName(c"frame".as_ptr()));
            let visible = rect_of(primary, sel_registerName(c"visibleFrame".as_ptr()));
            // Flip to top-left origin.
            let top = frame.size.height - (visible.origin.y + visible.size.height);
            Some(Rect::new(
                visible.origin.x.round() as i32,
                top.round() as i32,
                visible.size.width.round() as i32,
                visible.size.height.round() as i32,
            ))
        })();
        objc_autoreleasePoolPop(pool);
        result.filter(|r| r.w > 0 && r.h > 0)
    }
}

/// What the Dock is doing, as far as the tiled display is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DockState {
    /// Set to hide itself: windows may use the whole edge.
    Hidden,
    /// Shown, but on another display.
    Elsewhere,
    Shown(Dock),
    /// The Dock could not be asked; trust AppKit's visible frame.
    Unknown,
}

/// Reads the Dock's live state from the Dock itself. AppKit's visible frame
/// cannot be relied on for this: without an `NSApplication` run loop it is
/// only refreshed now and then, so it lags behind the Dock being shown,
/// hidden or moved.
struct DockApi {
    autohide: Option<CoreDockGetAutoHideEnabledFn>,
    orientation: Option<CoreDockGetOrientationAndPinningFn>,
    rect: Option<SLSGetDockRectWithReasonFn>,
    prefs_app: Cf,
    autohide_key: Cf,
}

/// Bringing an app forward with one of its windows made key. Asking an
/// app to activate through Accessibility is ignored unless the user just
/// pressed a key or clicked (macOS's "cooperative activation"), so focus
/// follows mouse would do nothing; this asks the window server directly.
struct FocusApi {
    psn: Option<GetProcessForPIDFn>,
    front: Option<SLPSSetFrontProcessWithOptionsFn>,
    post: Option<SLPSPostEventRecordToFn>,
}

impl FocusApi {
    fn new() -> Self {
        unsafe {
            Self {
                psn: lookup(c"GetProcessForPID"),
                front: lookup(c"_SLPSSetFrontProcessWithOptions"),
                post: lookup(c"SLPSPostEventRecordTo"),
            }
        }
    }

    /// Makes `pid` the front app and `wid` its key window. Returns whether
    /// it could.
    fn focus(&self, pid: i32, wid: WinId) -> bool {
        let (Some(get_psn), Some(front), Some(post)) = (self.psn, self.front, self.post) else {
            return false;
        };
        let mut psn = ProcessSerialNumber::default();
        unsafe {
            if get_psn(pid, &raw mut psn) != 0 {
                return false;
            }
            if front(&raw const psn, wid, CPS_USER_GENERATED) != 0 {
                return false;
            }
            // Two synthetic window-server events that make the window key.
            for kind in [1u8, 2u8] {
                let mut bytes = [0u8; 0xf8];
                bytes[0x04] = 0xf8;
                bytes[0x08] = kind;
                bytes[0x3a] = 0x10;
                bytes[0x20..0x30].fill(0xff);
                bytes[0x3c..0x40].copy_from_slice(&wid.to_le_bytes());
                post(&raw const psn, bytes.as_ptr());
            }
        }
        true
    }
}

/// Listing the Spaces and moving windows between them.
struct SpaceApi {
    list: Option<SLSCopyManagedDisplaySpacesFn>,
    compat: Option<SLSSpaceSetCompatIDFn>,
    workspace: Option<SLSSetWindowListWorkspaceFn>,
    managed_move: Option<SLSMoveWindowsToManagedSpaceFn>,
    display_key: Cf,
    spaces_key: Cf,
    id_key: Cf,
    type_key: Cf,
}

/// A tag no real Space uses, to address one Space while moving windows.
const MOVE_TAG: i32 = 0x6273_7078;

impl SpaceApi {
    fn new() -> Self {
        unsafe {
            Self {
                list: lookup(c"SLSCopyManagedDisplaySpaces"),
                compat: lookup(c"SLSSpaceSetCompatID"),
                workspace: lookup(c"SLSSetWindowListWorkspace"),
                managed_move: lookup(c"SLSMoveWindowsToManagedSpace"),
                display_key: cfstring("Display Identifier"),
                spaces_key: cfstring("Spaces"),
                id_key: cfstring("ManagedSpaceID"),
                type_key: cfstring("type"),
            }
        }
    }

    /// The desktops (not full-screen apps) of the display named `display`
    /// (or of the first one), in Mission Control order.
    fn spaces(&self, cid: i32, display: Option<&str>) -> Vec<u64> {
        let Some(list) = self.list else {
            return Vec::new();
        };
        let mut out = Vec::new();
        unsafe {
            let Some(displays) = Cf::owned(list(cid)) else {
                return out;
            };
            let count = CFArrayGetCount(displays.ptr());
            let chosen = (0..count)
                .map(|i| CFArrayGetValueAtIndex(displays.ptr(), i))
                .find(|d| {
                    let name = string_value(CFDictionaryGetValue(*d, self.display_key.ptr()));
                    display.is_some_and(|want| name.as_deref() == Some(want))
                })
                .or_else(|| (count > 0).then(|| CFArrayGetValueAtIndex(displays.ptr(), 0)));
            let Some(dict) = chosen else {
                return out;
            };
            let spaces = CFDictionaryGetValue(dict, self.spaces_key.ptr());
            if spaces.is_null() || CFGetTypeID(spaces) != CFArrayGetTypeID() {
                return out;
            }
            for i in 0..CFArrayGetCount(spaces) {
                let space = CFArrayGetValueAtIndex(spaces, i);
                if space.is_null() || CFGetTypeID(space) != CFDictionaryGetTypeID() {
                    continue;
                }
                let kind = i64_value(CFDictionaryGetValue(space, self.type_key.ptr()));
                if kind.unwrap_or(0) != 0 {
                    continue; // a full-screen app
                }
                if let Some(id) = i64_value(CFDictionaryGetValue(space, self.id_key.ptr())) {
                    out.push(id as u64);
                }
            }
        }
        out
    }

    /// Asks the window server to put `wid` on Space `sid`, without
    /// disabling SIP: the Space is given a temporary tag and the window
    /// is sent to that tag.
    fn move_by_tag(&self, cid: i32, wid: WinId, sid: u64) {
        if let (Some(compat), Some(workspace)) = (self.compat, self.workspace) {
            unsafe {
                compat(cid, sid, MOVE_TAG);
                workspace(cid, &raw const wid, 1, MOVE_TAG);
                compat(cid, sid, 0);
            }
        }
    }

    /// The direct call, which older systems allow.
    fn move_direct(&self, cid: i32, sid: u64, number: Option<Cf>) {
        unsafe {
            if let (Some(managed_move), Some(number)) = (self.managed_move, number) {
                let value = number.ptr();
                if let Some(array) = Cf::owned(CFArrayCreate(
                    ptr::null(),
                    &raw const value,
                    1,
                    &raw const kCFTypeArrayCallBacks,
                )) {
                    managed_move(cid, array.ptr(), sid);
                }
            }
        }
    }
}

impl MacBackend {
    /// Carries a window to another Space by holding its title bar while
    /// switching there with the keyboard shortcut. The user ends up there
    /// too; if the window does not come along, they are taken back.
    fn carry(&mut self, id: WinId, space: u64) -> bool {
        let spaces = self.spaces();
        let Some(n) = spaces.iter().position(|s| *s == space).map(|i| i + 1) else {
            return false;
        };
        let here = self.active_space();
        let from = spaces.iter().position(|s| *s == here).map(|i| i + 1);
        let Some(pid) = self.windows.get(&id).map(|w| w.pid) else {
            return false;
        };
        let Some(frame) = self.window_frame(id) else {
            return false;
        };
        let system = self.system.ptr();
        let result = desktop::carry(system, pid, id, frame, n, || {
            self.window_spaces(&[id]).get(&id) == Some(&space)
        });
        match result {
            Ok(()) => true,
            Err(e) => {
                warn!("{e}");
                if let Some(from) = from
                    && self.active_space() != here
                {
                    let _ = desktop::switch(from);
                }
                false
            }
        }
    }
}

/// The main display's UUID, as the Space functions name displays.
fn main_display_uuid() -> Option<Cf> {
    unsafe {
        let uuid = Cf::owned(CGDisplayCreateUUIDFromDisplayID(CGMainDisplayID()))?;
        Cf::owned(CFUUIDCreateString(ptr::null(), uuid.ptr()))
    }
}

/// Looks up a function in the loaded frameworks. `T` must be the
/// function's pointer type.
unsafe fn lookup<T: Copy>(name: &CStr) -> Option<T> {
    debug_assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    let p = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if p.is_null() {
        warn!("{} is unavailable", name.to_string_lossy());
        return None;
    }
    Some(unsafe { std::mem::transmute_copy(&p) })
}

impl DockApi {
    fn new() -> Self {
        unsafe {
            Self {
                autohide: lookup(c"CoreDockGetAutoHideEnabled"),
                orientation: lookup(c"CoreDockGetOrientationAndPinning"),
                rect: lookup(c"SLSGetDockRectWithReason"),
                prefs_app: cfstring("com.apple.dock"),
                autohide_key: cfstring("autohide"),
            }
        }
    }

    fn hides(&self) -> bool {
        if let Some(f) = self.autohide {
            return unsafe { f() } != 0;
        }
        unsafe {
            CFPreferencesAppSynchronize(self.prefs_app.ptr());
            Cf::owned(CFPreferencesCopyAppValue(
                self.autohide_key.ptr(),
                self.prefs_app.ptr(),
            ))
            .and_then(|v| bool_value(v.ptr()))
            .unwrap_or(false)
        }
    }

    fn state(&self, connection: i32, display: Rect) -> DockState {
        if self.hides() {
            return DockState::Hidden;
        }
        let Some(get_rect) = self.rect else {
            return DockState::Unknown;
        };
        let mut cg = CGRect::default();
        let mut reason = 0;
        if unsafe { get_rect(connection, &mut cg, &mut reason) } != 0 {
            return DockState::Unknown;
        }
        let r = cg_to_rect(cg);
        if r.w <= 0 || r.h <= 0 {
            return DockState::Unknown;
        }
        let (cx, cy) = r.center();
        if !display.contains_point(cx, cy) {
            return DockState::Elsewhere;
        }
        let orientation = self.orientation.map(|f| {
            let (mut orientation, mut pinning) = (0, 0);
            unsafe { f(&mut orientation, &mut pinning) };
            orientation
        });
        let edge = match orientation {
            Some(DOCK_ORIENTATION_BOTTOM) => DockEdge::Bottom,
            Some(DOCK_ORIENTATION_LEFT) => DockEdge::Left,
            Some(DOCK_ORIENTATION_RIGHT) => DockEdge::Right,
            _ if r.w >= r.h => DockEdge::Bottom,
            _ if cx < display.center().0 => DockEdge::Left,
            _ => DockEdge::Right,
        };
        let thickness = match edge {
            DockEdge::Bottom => r.h,
            DockEdge::Left | DockEdge::Right => r.w,
        };
        DockState::Shown(Dock { edge, thickness })
    }
}

thread_local! {
    /// Filled by `ax_callback` while the run loop is pumped (main thread only).
    static EVENTS: std::cell::RefCell<Vec<WindowEvent>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `refcon` tags: a window's destruction carries its id; anything else
/// (minimise, restore, hide, show) only asks for a fresh look.
const CHANGED_TAG: usize = 1 << 40;

unsafe extern "C" fn ax_callback(
    _observer: AXObserverRef,
    _element: AXUIElementRef,
    _notification: CFStringRef,
    refcon: *mut c_void,
) {
    let tag = refcon as usize;
    let event = if tag & CHANGED_TAG != 0 {
        WindowEvent::Changed
    } else {
        WindowEvent::Destroyed(tag as WinId)
    };
    EVENTS.with(|e| e.borrow_mut().push(event));
}

/// An application's AX element and the observer delivering its window events.
struct AppHandle {
    element: Cf,
    observer: Option<Cf>,
}

impl Drop for AppHandle {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            unsafe {
                let source = AXObserverGetRunLoopSource(observer.ptr());
                if !source.is_null() {
                    CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                }
            }
        }
    }
}

fn observe(observer: &Cf, element: AXUIElementRef, notification: &Cf, refcon: usize) {
    unsafe {
        AXObserverAddNotification(
            observer.ptr(),
            element,
            notification.ptr(),
            refcon as *mut c_void,
        );
    }
}

struct TrackedWindow {
    element: Cf,
    pid: i32,
    tileable: bool,
}

/// How many polls to keep looking for the AX element of a new CG window
/// before treating it as unmanageable.
const MAX_LOOKUPS: u8 = 20;

pub struct MacBackend {
    names: Names,
    system: Cf,
    apps: HashMap<i32, AppHandle>,
    windows: HashMap<WinId, TrackedWindow>,
    lookups: HashMap<WinId, u8>,
    display: Rect,
    /// Windows that exist on any Space, as of the last `windows()` call.
    alive: HashSet<WinId>,
    connection: i32,
    /// Created the first time a drop target is highlighted.
    overlay: Option<OverlayWindow>,
    dock: DockApi,
    focuser: FocusApi,
    space_api: SpaceApi,
    /// The last tiling area reported, to log changes.
    area: Rect,
    hotkeys: hotkeys::HotkeyManager,
}

impl Default for MacBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MacBackend {
    pub fn new() -> Self {
        let system =
            Cf::owned(unsafe { AXUIElementCreateSystemWide() }).expect("system-wide AX element");
        unsafe { AXUIElementSetMessagingTimeout(system.ptr(), 0.25) };
        Self {
            names: Names::new(),
            system,
            apps: HashMap::new(),
            windows: HashMap::new(),
            lookups: HashMap::new(),
            display: Rect::default(),
            alive: HashSet::new(),
            connection: unsafe { CGSMainConnectionID() },
            overlay: None,
            dock: DockApi::new(),
            focuser: FocusApi::new(),
            space_api: SpaceApi::new(),
            area: Rect::default(),
            hotkeys: hotkeys::HotkeyManager::default(),
        }
    }

    fn app(&mut self, pid: i32) -> Option<AXUIElementRef> {
        if !self.apps.contains_key(&pid) {
            let app = Cf::owned(unsafe { AXUIElementCreateApplication(pid) })?;
            unsafe { AXUIElementSetMessagingTimeout(app.ptr(), 0.25) };
            // With "enhanced user interface" on (set by some accessibility
            // tools), apps animate every frame change themselves, which
            // fights our animation and makes moves sluggish.
            if copy_attr(app.ptr(), &self.names.enhanced_ui).and_then(|v| bool_value(v.ptr()))
                == Some(true)
            {
                set_attr(app.ptr(), &self.names.enhanced_ui, unsafe {
                    kCFBooleanFalse
                });
            }
            let observer = Self::create_observer(pid);
            if let Some(obs) = &observer {
                observe(obs, app.ptr(), &self.names.app_hidden, CHANGED_TAG);
                observe(obs, app.ptr(), &self.names.app_shown, CHANGED_TAG);
            }
            self.apps.insert(
                pid,
                AppHandle {
                    element: app,
                    observer,
                },
            );
        }
        self.apps.get(&pid).map(|a| a.element.ptr())
    }

    /// An observer for `pid`, attached to this thread's run loop.
    fn create_observer(pid: i32) -> Option<Cf> {
        let mut raw: AXObserverRef = ptr::null();
        unsafe {
            if AXObserverCreate(pid, ax_callback, &mut raw) != kAXErrorSuccess {
                debug!("no AX observer for pid {pid}");
                return None;
            }
            let observer = Cf::owned(raw)?;
            let source = AXObserverGetRunLoopSource(observer.ptr());
            if source.is_null() {
                return None;
            }
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            Some(observer)
        }
    }

    /// Looks up the AX elements of an app's windows and tracks those in `wanted`.
    fn discover(&mut self, pid: i32, wanted: &HashSet<WinId>) {
        let Some(app) = self.app(pid) else { return };
        let Some(list) = copy_attr(app, &self.names.windows) else {
            return;
        };
        unsafe {
            if CFGetTypeID(list.ptr()) != CFArrayGetTypeID() {
                return;
            }
            for i in 0..CFArrayGetCount(list.ptr()) {
                let element = CFArrayGetValueAtIndex(list.ptr(), i);
                let Some(id) = window_id(element) else {
                    continue;
                };
                if !wanted.contains(&id) || self.windows.contains_key(&id) {
                    continue;
                }
                let Some(element) = Cf::retained(element) else {
                    continue;
                };
                let tileable = self.is_tileable(element.ptr());
                debug!("tracking window {id} (pid {pid}, tileable {tileable})");
                if let Some(obs) = self.apps.get(&pid).and_then(|a| a.observer.as_ref()) {
                    observe(obs, element.ptr(), &self.names.destroyed, id as usize);
                    observe(obs, element.ptr(), &self.names.miniaturized, CHANGED_TAG);
                    observe(obs, element.ptr(), &self.names.deminiaturized, CHANGED_TAG);
                }
                self.windows.insert(
                    id,
                    TrackedWindow {
                        element,
                        pid,
                        tileable,
                    },
                );
            }
        }
    }

    fn is_tileable(&self, element: AXUIElementRef) -> bool {
        let subrole = copy_attr(element, &self.names.subrole).and_then(|v| string_value(v.ptr()));
        if subrole.as_deref() != Some(self.names.standard_window) {
            return false;
        }
        if copy_attr(element, &self.names.fullscreen).and_then(|v| bool_value(v.ptr()))
            == Some(true)
        {
            return false;
        }
        is_settable(element, &self.names.size) && is_settable(element, &self.names.position)
    }
}

impl MacBackend {
    fn write_frame(
        &mut self,
        id: WinId,
        frame: Rect,
        moved: bool,
        resized: bool,
        size_first: bool,
    ) {
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let element = win.element.ptr();
        let point = CGPoint {
            x: f64::from(frame.x),
            y: f64::from(frame.y),
        };
        let size = CGSize {
            width: f64::from(frame.w),
            height: f64::from(frame.h),
        };
        let set_position = || unsafe {
            if let Some(v) = Cf::owned(AXValueCreate(
                kAXValueTypeCGPoint,
                (&raw const point).cast(),
            )) {
                set_attr(element, &self.names.position, v.ptr());
            }
        };
        let set_size = || unsafe {
            if let Some(v) = Cf::owned(AXValueCreate(kAXValueTypeCGSize, (&raw const size).cast()))
            {
                set_attr(element, &self.names.size, v.ptr());
            }
        };
        if size_first {
            if resized {
                set_size();
            }
            if moved {
                set_position();
            }
        } else {
            if moved {
                set_position();
            }
            if resized {
                set_size();
            }
        }
    }
}

impl Backend for MacBackend {
    fn hotkey_commands(&mut self) -> Vec<String> {
        self.hotkeys.poll();
        self.hotkeys.take_lines()
    }

    fn set_hotkeys(&mut self, text: &str) {
        self.hotkeys.set(text);
    }

    fn windows(&mut self) -> Vec<WindowInfo> {
        let cg = cg_windows(true);
        let all = cg_windows(false);
        let present: HashSet<WinId> = cg.iter().map(|w| w.id).collect();
        self.alive = all
            .iter()
            .map(|w| w.id)
            .chain(present.iter().copied())
            .collect();
        // Keep the AX handles of windows on other Spaces: they stay valid,
        // and the app only lists the windows of the active Space.
        let alive = &self.alive;
        self.windows.retain(|id, _| alive.contains(id));
        self.lookups.retain(|id, _| present.contains(id));
        let live_pids: HashSet<i32> = all.iter().chain(&cg).map(|w| w.pid).collect();
        self.apps.retain(|pid, _| live_pids.contains(pid));

        // Ask each app with unknown windows for its AX windows, once per poll.
        let mut unknown: HashMap<i32, HashSet<WinId>> = HashMap::new();
        for w in &cg {
            if !self.windows.contains_key(&w.id)
                && self.lookups.get(&w.id).copied().unwrap_or(0) < MAX_LOOKUPS
            {
                unknown.entry(w.pid).or_default().insert(w.id);
            }
        }
        for (pid, ids) in unknown {
            if pid == std::process::id() as i32 {
                continue;
            }
            self.discover(pid, &ids);
            for id in ids.into_iter().filter(|id| !self.windows.contains_key(id)) {
                *self.lookups.entry(id).or_default() += 1;
            }
        }

        let display = self.display;
        cg.into_iter()
            .map(|w| {
                let (cx, cy) = w.frame.center();
                let on_tiled_display = display.w == 0 || display.contains_point(cx, cy);
                let tileable =
                    on_tiled_display && self.windows.get(&w.id).is_some_and(|t| t.tileable);
                WindowInfo {
                    id: w.id,
                    frame: w.frame,
                    app: w.app,
                    tileable,
                }
            })
            .collect()
    }

    fn focused_window(&mut self) -> Option<WinId> {
        let ax = copy_attr(self.system.ptr(), &self.names.focused_application)
            .and_then(|app| copy_attr(app.ptr(), &self.names.focused_window))
            .and_then(|window| window_id(window.ptr()));
        // The Accessibility API sometimes cannot answer (a busy app, a
        // timeout). The window in front is then the best guess: focusing a
        // window raises it, and CoreGraphics lists windows front to back.
        ax.or_else(|| cg_windows(true).first().map(|w| w.id))
    }

    fn tiling_area(&mut self) -> Rect {
        let full = cg_to_rect(unsafe { CGDisplayBounds(CGMainDisplayID()) });
        self.display = full;
        let visible = primary_screen_area().unwrap_or_else(|| {
            warn!("NSScreen unavailable; assuming a 25pt menu bar");
            Rect::new(full.x, full.y + 25, full.w, full.h - 25)
        });
        let dock = self.dock.state(self.connection, full);
        let area = match dock {
            DockState::Shown(d) => usable_area(full, visible, Some(d)),
            DockState::Hidden | DockState::Elsewhere => usable_area(full, visible, None),
            DockState::Unknown => visible,
        };
        if area != self.area {
            info!("tiling area {area:?} (dock: {dock:?}, visible frame: {visible:?})");
            self.area = area;
        }
        area
    }

    fn set_frame(&mut self, id: WinId, frame: Rect) {
        self.write_frame(id, frame, true, true, false);
    }

    fn update_frame(&mut self, id: WinId, frame: Rect, prev: Rect) {
        let moved = (frame.x, frame.y) != (prev.x, prev.y);
        let resized = (frame.w, frame.h) != (prev.w, prev.h);
        // Growing: move first, so the larger window is not pushed back by the
        // screen edge. Shrinking: resize first, for the same reason.
        let grows = frame.w > prev.w || frame.h > prev.h;
        self.write_frame(id, frame, moved, resized, !grows);
    }

    fn active_space(&mut self) -> u64 {
        // The Space on the main display, the one bspx tiles. The system-wide
        // "active" Space follows focus to other displays instead.
        unsafe {
            let space = main_display_uuid().map_or(0, |n| {
                CGSManagedDisplayGetCurrentSpace(self.connection, n.ptr())
            });
            if space != 0 {
                space
            } else {
                CGSGetActiveSpace(self.connection)
            }
        }
    }

    fn alive_windows(&mut self) -> Option<HashSet<WinId>> {
        Some(self.alive.clone())
    }

    fn window_spaces(&mut self, ids: &[WinId]) -> HashMap<WinId, u64> {
        let mut out = HashMap::with_capacity(ids.len());
        for &id in ids {
            unsafe {
                let wid = i64::from(id);
                let Some(number) = Cf::owned(CFNumberCreate(
                    ptr::null(),
                    kCFNumberSInt64Type,
                    (&raw const wid).cast(),
                )) else {
                    continue;
                };
                let value = number.ptr();
                let Some(array) = Cf::owned(CFArrayCreate(
                    ptr::null(),
                    &raw const value,
                    1,
                    &raw const kCFTypeArrayCallBacks,
                )) else {
                    continue;
                };
                let Some(spaces) = Cf::owned(CGSCopySpacesForWindows(
                    self.connection,
                    kCGSAllSpacesMask,
                    array.ptr(),
                )) else {
                    continue;
                };
                // A window on several Spaces ("all desktops") counts as on the
                // first; minimised windows and hidden apps are on none.
                if CFArrayGetCount(spaces.ptr()) > 0
                    && let Some(space) = i64_value(CFArrayGetValueAtIndex(spaces.ptr(), 0))
                {
                    out.insert(id, space as u64);
                }
            }
        }
        out
    }

    fn spaces(&mut self) -> Vec<u64> {
        let display = main_display_uuid().and_then(|u| string_value(u.ptr()));
        let mut spaces = self.space_api.spaces(self.connection, display.as_deref());
        if spaces.is_empty() {
            spaces.push(self.active_space());
        }
        spaces
    }

    fn move_to_space(&mut self, id: WinId, space: u64) -> bool {
        let wid = i64::from(id);
        let number = Cf::owned(unsafe {
            CFNumberCreate(ptr::null(), kCFNumberSInt64Type, (&raw const wid).cast())
        });
        // Trust only what the window server says afterwards.
        self.space_api.move_by_tag(self.connection, id, space);
        let mut moved = self.window_spaces(&[id]).get(&id) == Some(&space);
        if !moved {
            self.space_api.move_direct(self.connection, space, number);
            moved = self.window_spaces(&[id]).get(&id) == Some(&space);
        }
        if !moved {
            moved = self.carry(id, space);
        }
        if !moved {
            warn!("macOS did not move window {id} to Space {space}");
        }
        moved
    }

    fn switch_to_space(&mut self, n: usize) -> Result<(), String> {
        desktop::switch(n)
    }

    fn set_mouse_modifier(&mut self, modifier: Option<Modifier>) {
        tap::set_modifier(modifier);
    }

    fn warp_cursor(&mut self, x: f32, y: f32) {
        unsafe {
            CGWarpMouseCursorPosition(CGPoint {
                x: f64::from(x),
                y: f64::from(y),
            });
            // A warp freezes the pointer for a moment unless it is
            // reconnected to the mouse right away.
            CGAssociateMouseAndMouseCursorPosition(1);
        }
    }

    fn covered_at(&mut self, x: f32, y: f32) -> bool {
        covered_at(x, y)
    }

    fn panel_has_focus(&mut self) -> bool {
        let pid = copy_attr(self.system.ptr(), &self.names.focused_application).and_then(|app| {
            let mut pid = 0;
            (unsafe { AXUIElementGetPid(app.ptr(), &raw mut pid) } == 0).then_some(pid)
        });
        shows_panel(pid)
    }

    fn mouse_gesture(&mut self) -> Option<MouseButton> {
        tap::gesture()
    }

    fn cursor(&mut self) -> Option<(f32, f32)> {
        if let Some(p) = tap::location() {
            return Some(p);
        }
        let event = Cf::owned(unsafe { CGEventCreate(ptr::null()) })?;
        let p = unsafe { CGEventGetLocation(event.ptr()) };
        Some((p.x as f32, p.y as f32))
    }

    fn window_frame(&mut self, id: WinId) -> Option<Rect> {
        let element = self.windows.get(&id)?.element.ptr();
        let mut point = CGPoint::default();
        let mut size = CGSize::default();
        unsafe {
            let pos = copy_attr(element, &self.names.position)?;
            let sz = copy_attr(element, &self.names.size)?;
            if AXValueGetValue(pos.ptr(), kAXValueTypeCGPoint, (&raw mut point).cast()) == 0
                || AXValueGetValue(sz.ptr(), kAXValueTypeCGSize, (&raw mut size).cast()) == 0
            {
                return None;
            }
        }
        Some(cg_to_rect(CGRect {
            origin: point,
            size,
        }))
    }

    fn show_overlay(&mut self, overlay: Option<Overlay>) {
        match overlay {
            Some(o) => {
                if self.overlay.is_none() {
                    self.overlay = OverlayWindow::new();
                    if self.overlay.is_none() {
                        warn!("cannot create the drag highlight window");
                    }
                }
                if let Some(w) = &mut self.overlay {
                    w.show(o.rect, o.above, o.color);
                }
            }
            None => {
                if let Some(w) = &mut self.overlay {
                    w.hide();
                }
            }
        }
    }

    fn pump(&mut self) {
        // Deliver pending AX notifications (they arrive as run loop sources).
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0) };
        if let Some(w) = &mut self.overlay {
            w.pump();
        }
    }

    fn take_events(&mut self) -> Vec<WindowEvent> {
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        for event in &events {
            if let WindowEvent::Destroyed(id) = event {
                // Its AX element is dead; never tile it again.
                self.windows.remove(id);
            }
        }
        events
    }

    fn mouse_down(&mut self) -> bool {
        unsafe {
            CGEventSourceButtonState(kCGEventSourceStateCombinedSessionState, kCGMouseButtonLeft)
        }
    }

    fn focus(&mut self, id: WinId) {
        let Some(win) = self.windows.get(&id) else {
            return;
        };
        let (element, pid) = (win.element.ptr(), win.pid);
        let focused = self.focuser.focus(pid, id);
        unsafe {
            AXUIElementPerformAction(element, self.names.raise.ptr());
            set_attr(element, &self.names.main, kCFBooleanTrue);
        }
        if !focused && let Some(app) = self.app(pid) {
            set_attr(app, &self.names.frontmost, unsafe { kCFBooleanTrue });
        }
    }
}
