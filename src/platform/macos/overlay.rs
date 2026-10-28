//! The drop-target highlight: a borderless, click-through AppKit window with
//! a rounded, translucent layer, stacked just above the target window (and
//! so below the window being dragged, which is frontmost).

use std::ffi::c_void;
use std::ptr;

use super::ffi::*;
use crate::bsp::WinId;
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
const CORNER_RADIUS: f64 = 10.0;
const BORDER_WIDTH: f64 = 2.0;

pub struct OverlayWindow {
    app: Id,
    window: Id,
    layer: Id,
    color: Option<[f32; 4]>,
    visible: bool,
}

impl OverlayWindow {
    pub fn new() -> Option<Self> {
        unsafe {
            let app: Id = msg!(class(c"NSApplication"), "sharedApplication"; -> Id);
            if app.is_null() {
                return None;
            }
            // No Dock icon, no menu bar: bspx stays a background tool.
            let _: i8 = msg!(app, "setActivationPolicy:", NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY => isize; -> i8);
            msg!(app, "finishLaunching");

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
            msg!(window, "setCollectionBehavior:", COLLECTION_BEHAVIOR => usize);
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
                visible: false,
            })
        }
    }

    /// Shows the highlight over `rect` (top-left global coordinates), just
    /// above window `above`.
    pub fn show(&mut self, rect: Rect, above: WinId, color: [f32; 4]) {
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
            msg!(self.window, "orderWindow:relativeTo:", NS_WINDOW_ABOVE => isize, above as isize => isize);
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
    /// stays responsive. bspx has no run loop of its own to do this.
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
