//! The drop-target highlight and the ring around the focused window:
//! borderless, click-through AppKit windows with a rounded layer, above
//! every normal window. macOS does not reliably stack them right above a
//! window of another app: they would end up behind it.

use std::ffi::c_void;
use std::ptr;
use std::sync::Once;

use super::ffi::*;
use crate::geometry::Rect;

/// `objc_msgSend` cast to the method's real signature.
macro_rules! msg {
    ($obj:expr, $sel:literal $(, $arg:expr => $ty:ty)* ; -> $ret:ty) => {{
        let f: unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f($obj, sel_registerName(concat!($sel, "\0").as_ptr().cast()) $(, $arg)*)
    }};
    ($obj:expr, $sel:literal $(, $arg:expr => $ty:ty)*) => {
        msg!($obj, $sel $(, $arg => $ty)*; -> ())
    };
}

unsafe fn class(name: &std::ffi::CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

const YES: i8 = 1;
const NO: i8 = 0;
const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;
const NS_BACKING_STORE_BUFFERED: usize = 2;
const NS_WINDOW_STYLE_MASK_BORDERLESS: usize = 0;
const NS_WINDOW_ABOVE: isize = 1;
const NS_WINDOW_ANIMATION_BEHAVIOR_NONE: isize = 2;
/// canJoinAllSpaces | stationary | ignoresCycle | fullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 4) | (1 << 6) | (1 << 8);
/// moveToActiveSpace | transient | ignoresCycle | fullScreenAuxiliary: the
/// ring stays on the Space of its window, so a swipe takes it away with the
/// window, and Mission Control hides it.
const RING_BEHAVIOR: usize = (1 << 1) | (1 << 3) | (1 << 6) | (1 << 8);
/// `NSFloatingWindowLevel`: above every normal window, so that no click can
/// raise a window, or its shadow, over these.
const NS_FLOATING_WINDOW_LEVEL: isize = 3;
const CORNER_RADIUS: f64 = 10.0;
const BORDER_WIDTH: f64 = 2.0;

pub struct OverlayWindow {
    app: Id,
    window: Id,
    layer: Id,
    color: Option<[f32; 4]>,
    /// The width and radius a ring was last drawn with.
    ring: Option<(f64, f64)>,
    visible: bool,
}

impl OverlayWindow {
    /// The drop highlight, which stays put whatever the Spaces do.
    pub fn new() -> Option<Self> {
        Self::with_behavior(COLLECTION_BEHAVIOR)
    }

    /// The ring around the focused window.
    pub fn ring() -> Option<Self> {
        Self::with_behavior(RING_BEHAVIOR)
    }

    fn with_behavior(behavior: usize) -> Option<Self> {
        unsafe {
            let app: Id = msg!(class(c"NSApplication"), "sharedApplication"; -> Id);
            if app.is_null() {
                return None;
            }
            // No Dock icon, no menu bar: cberry stays a background tool.
            let _: i8 = msg!(app, "setActivationPolicy:", NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY => isize; -> i8);
            static LAUNCH: Once = Once::new();
            LAUNCH.call_once(|| msg!(app, "finishLaunching"));

            let window: Id = msg!(class(c"NSWindow"), "alloc"; -> Id);
            let window: Id = msg!(window, "initWithContentRect:styleMask:backing:defer:",
                CGRect::default() => CGRect,
                NS_WINDOW_STYLE_MASK_BORDERLESS => usize,
                NS_BACKING_STORE_BUFFERED => usize,
                NO => i8;
                -> Id);
            if window.is_null() {
                return None;
            }
            msg!(window, "setReleasedWhenClosed:", NO => i8);
            msg!(window, "setOpaque:", NO => i8);
            msg!(window, "setHasShadow:", NO => i8);
            msg!(window, "setIgnoresMouseEvents:", YES => i8);
            let clear: Id = msg!(class(c"NSColor"), "clearColor"; -> Id);
            msg!(window, "setBackgroundColor:", clear => Id);
            msg!(window, "setCollectionBehavior:", behavior => usize);
            msg!(window, "setLevel:", NS_FLOATING_WINDOW_LEVEL => isize);
            msg!(window, "setAnimationBehavior:", NS_WINDOW_ANIMATION_BEHAVIOR_NONE => isize);

            let view: Id = msg!(window, "contentView"; -> Id);
            msg!(view, "setWantsLayer:", YES => i8);
            let layer: Id = msg!(view, "layer"; -> Id);
            if layer.is_null() {
                return None;
            }
            msg!(layer, "setCornerRadius:", CORNER_RADIUS => f64);
            msg!(layer, "setBorderWidth:", BORDER_WIDTH => f64);
            Some(Self {
                app,
                window,
                layer,
                color: None,
                ring: None,
                visible: false,
            })
        }
    }

    /// Shows the highlight over `rect` (top-left global coordinates).
    pub fn show(&mut self, rect: Rect, color: [f32; 4]) {
        unsafe {
            let catx = class(c"CATransaction");
            msg!(catx, "begin");
            msg!(catx, "setDisableActions:", YES => i8);
            if self.color != Some(color) {
                let [r, g, b, a] = color.map(f64::from);
                let fill = CGColorCreateSRGB(r, g, b, a);
                let border = CGColorCreateSRGB(r, g, b, (a * 3.0).min(1.0));
                msg!(self.layer, "setBackgroundColor:", fill => CFTypeRef);
                msg!(self.layer, "setBorderColor:", border => CFTypeRef);
                CGColorRelease(fill);
                CGColorRelease(border);
                self.color = Some(color);
            }
            msg!(catx, "commit");
            self.place(rect);
        }
    }

    /// Shows a ring just outside `rect` (top-left global coordinates).
    /// `radius` is the roundness of the window's own corners, so that the
    /// ring follows them.
    pub fn show_ring(&mut self, rect: Rect, color: [f32; 4], width: i32, radius: i32) {
        let outer = Rect::new(
            rect.x - width,
            rect.y - width,
            rect.w + 2 * width,
            rect.h + 2 * width,
        );
        let ring = (f64::from(width), f64::from(radius + width));
        unsafe {
            let catx = class(c"CATransaction");
            msg!(catx, "begin");
            msg!(catx, "setDisableActions:", YES => i8);
            if self.color != Some(color) || self.ring != Some(ring) {
                let [r, g, b, a] = color.map(f64::from);
                let border = CGColorCreateSRGB(r, g, b, a);
                msg!(self.layer, "setBorderColor:", border => CFTypeRef);
                CGColorRelease(border);
                msg!(self.layer, "setBorderWidth:", ring.0 => f64);
                msg!(self.layer, "setCornerRadius:", ring.1 => f64);
                self.color = Some(color);
                self.ring = Some(ring);
            }
            msg!(catx, "commit");
            self.place(outer);
        }
    }

    /// Puts the window over `rect`, in front of its level.
    unsafe fn place(&mut self, rect: Rect) {
        unsafe {
            let catx = class(c"CATransaction");
            // Cocoa puts the origin at the bottom-left of the primary display.
            let screen_h = CGDisplayBounds(CGMainDisplayID()).size.height;
            let frame = CGRect {
                origin: CGPoint {
                    x: f64::from(rect.x),
                    y: screen_h - f64::from(rect.y + rect.h),
                },
                size: CGSize {
                    width: f64::from(rect.w),
                    height: f64::from(rect.h),
                },
            };
            msg!(self.window, "setFrame:display:", frame => CGRect, YES => i8);
            msg!(self.window, "orderWindow:relativeTo:", NS_WINDOW_ABOVE => isize, 0 => isize);
            msg!(catx, "flush");
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe { msg!(self.window, "orderOut:", ptr::null_mut::<c_void>() => Id) };
            self.visible = false;
        }
    }

    /// Hands pending AppKit events to the app so the window draws and
    /// stays responsive. cberry has no run loop of its own to do this.
    pub fn pump(&mut self) {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let distant_past: Id = msg!(class(c"NSDate"), "distantPast"; -> Id);
            loop {
                let event: Id = msg!(self.app, "nextEventMatchingMask:untilDate:inMode:dequeue:",
                    usize::MAX => usize,
                    distant_past => Id,
                    NSDefaultRunLoopMode => Id,
                    YES => i8;
                    -> Id);
                if event.is_null() {
                    break;
                }
                msg!(self.app, "sendEvent:", event => Id);
            }
            objc_autoreleasePoolPop(pool);
        }
    }
}
