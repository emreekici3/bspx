//! What tells bspx that something changed, now that it no longer polls.
//!
//! Window-level changes (a window opened, moved, resized, focused, closed,
//! minimised) come through each app's Accessibility observer, set up in
//! `mod.rs`. This module covers what no single app reports:
//!
//! * `NSWorkspace` notifications: an app launched (so its windows can be
//!   watched before it opens any), quit or came to the front, and the
//!   Space changed;
//! * display reconfiguration: a display added, removed or rearranged, or
//!   its resolution changed.
//!
//! Both arrive through the main thread's run loop, which `pump` runs every
//! frame. They only raise flags here; `take_events` turns them into events.

use std::ffi::{CStr, c_void};
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use log::{info, warn};

use super::cfstring;
use super::ffi::*;

/// Something changed that needs a fresh look at the window list.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// An app launched: look for new apps to watch. Set at start too, so the
/// apps already running are watched even if they have no window yet.
static RESCAN: AtomicBool = AtomicBool::new(true);

/// Whether something changed since the last call.
pub fn take_changed() -> bool {
    CHANGED.swap(false, Relaxed)
}

/// Whether apps were launched since the last call (true the first time).
pub fn take_rescan() -> bool {
    RESCAN.swap(false, Relaxed)
}

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

unsafe extern "C" fn changed(_this: Id, _cmd: Sel, _notification: Id) {
    CHANGED.store(true, Relaxed);
}

unsafe extern "C" fn launched(_this: Id, _cmd: Sel, _notification: Id) {
    RESCAN.store(true, Relaxed);
    CHANGED.store(true, Relaxed);
}

/// `kCGDisplayBeginConfigurationFlag`: the call made before the change.
const BEGIN_CONFIGURATION: u32 = 1;

unsafe extern "C" fn display_changed(_display: u32, flags: u32, _info: *mut c_void) {
    if flags & BEGIN_CONFIGURATION == 0 {
        CHANGED.store(true, Relaxed);
    }
}

/// Starts listening, once. Must run on the main thread, whose run loop
/// delivers the notifications.
pub fn start() {
    static START: Once = Once::new();
    START.call_once(|| match unsafe { listen() } {
        Ok(()) => info!("listening for app, Space and display changes"),
        Err(e) => warn!("no app, Space or display notifications: {e}"),
    });
}

unsafe fn sel(name: &CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

unsafe fn listen() -> Result<(), &'static str> {
    unsafe {
        NSApplicationLoad();

        // A tiny Objective-C class whose methods only raise the flags above.
        let superclass = objc_getClass(c"NSObject".as_ptr());
        let class = objc_allocateClassPair(superclass, c"BspxWorkspaceObserver".as_ptr(), 0);
        if class.is_null() {
            return Err("cannot create the observer class");
        }
        // Returns void; takes self, _cmd and the NSNotification.
        let types = c"v@:@".as_ptr();
        type Method = unsafe extern "C" fn(Id, Sel, Id);
        class_addMethod(
            class,
            sel(c"changed:"),
            changed as Method as *const c_void,
            types,
        );
        class_addMethod(
            class,
            sel(c"launched:"),
            launched as Method as *const c_void,
            types,
        );
        objc_registerClassPair(class);
        let observer: Id = msg!(class, "new"; -> Id);
        if observer.is_null() {
            return Err("cannot create the observer");
        }

        let workspace: Id = msg!(objc_getClass(c"NSWorkspace".as_ptr()), "sharedWorkspace"; -> Id);
        let center: Id = msg!(workspace, "notificationCenter"; -> Id);
        if center.is_null() {
            return Err("no NSWorkspace notification center");
        }
        let subscriptions: [(&str, &CStr); 4] = [
            ("NSWorkspaceDidLaunchApplicationNotification", c"launched:"),
            (
                "NSWorkspaceDidTerminateApplicationNotification",
                c"changed:",
            ),
            ("NSWorkspaceDidActivateApplicationNotification", c"changed:"),
            ("NSWorkspaceActiveSpaceDidChangeNotification", c"changed:"),
        ];
        for (name, selector) in subscriptions {
            // The notification center keeps its own reference to the name.
            let name = cfstring(name);
            msg!(center, "addObserver:selector:name:object:",
                observer => Id,
                sel(selector) => Sel,
                name.ptr().cast_mut() => Id,
                ptr::null_mut() => Id);
        }

        if CGDisplayRegisterReconfigurationCallback(display_changed, ptr::null_mut()) != 0 {
            warn!("no display change notifications");
        }
        Ok(())
    }
}

/// `NSApplicationActivationPolicyRegular`: an app with a Dock icon and
/// windows, as opposed to agents and background helpers.
const ACTIVATION_POLICY_REGULAR: isize = 0;

/// The process ids of the regular apps running now.
pub fn running_app_pids() -> Vec<i32> {
    let mut pids = Vec::new();
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let workspace: Id = msg!(objc_getClass(c"NSWorkspace".as_ptr()), "sharedWorkspace"; -> Id);
        let apps: Id = if workspace.is_null() {
            ptr::null_mut()
        } else {
            msg!(workspace, "runningApplications"; -> Id)
        };
        if !apps.is_null() {
            let count: usize = msg!(apps, "count"; -> usize);
            for i in 0..count {
                let app: Id = msg!(apps, "objectAtIndex:", i => usize; -> Id);
                if app.is_null() {
                    continue;
                }
                let policy: isize = msg!(app, "activationPolicy"; -> isize);
                if policy == ACTIVATION_POLICY_REGULAR {
                    pids.push(msg!(app, "processIdentifier"; -> i32));
                }
            }
        }
        objc_autoreleasePoolPop(pool);
    }
    pids
}
