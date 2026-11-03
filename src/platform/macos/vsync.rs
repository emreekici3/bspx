//! Waking up with the display. Sleeping for a frame's length drifts against
//! the refresh, so a frame now and then misses it and the next one covers
//! two: a hitch nobody can quite name. A display link ticks with the refresh.
//!
//! It only runs while something is moving. Anything that goes wrong with it
//! just means waiting by the clock, as a timeout.

use std::ffi::c_void;
use std::ptr;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use log::{debug, warn};

type CVDisplayLinkRef = *mut c_void;
type OutputCallback = unsafe extern "C" fn(
    CVDisplayLinkRef,
    *const c_void,
    *const c_void,
    u64,
    *mut u64,
    *mut c_void,
) -> i32;

#[cfg_attr(target_os = "macos", link(name = "CoreVideo", kind = "framework"))]
unsafe extern "C" {
    fn CVDisplayLinkCreateWithActiveCGDisplays(link: *mut CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkSetOutputCallback(
        link: CVDisplayLinkRef,
        callback: OutputCallback,
        context: *mut c_void,
    ) -> i32;
    fn CVDisplayLinkStart(link: CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkStop(link: CVDisplayLinkRef) -> i32;
}

/// Refreshes so far.
static TICKS: Mutex<u64> = Mutex::new(0);
static TICKED: Condvar = Condvar::new();

unsafe extern "C" fn tick(
    _link: CVDisplayLinkRef,
    _now: *const c_void,
    _output: *const c_void,
    _flags_in: u64,
    _flags_out: *mut u64,
    _context: *mut c_void,
) -> i32 {
    if let Ok(mut ticks) = TICKS.lock() {
        *ticks = ticks.wrapping_add(1);
        TICKED.notify_all();
    }
    0
}

#[derive(Default)]
enum Link {
    #[default]
    NotYet,
    Off(CVDisplayLinkRef),
    On(CVDisplayLinkRef),
    Unavailable,
}

#[derive(Default)]
pub struct Vsync {
    link: Link,
    /// The refresh the last wait ended on.
    seen: u64,
}

impl Vsync {
    fn start(&mut self) -> bool {
        unsafe {
            let link = match self.link {
                Link::On(_) => return true,
                Link::Unavailable => return false,
                Link::Off(link) => link,
                Link::NotYet => {
                    let mut link = ptr::null_mut();
                    let status = CVDisplayLinkCreateWithActiveCGDisplays(&raw mut link);
                    if status != 0 || link.is_null() {
                        warn!("no display link (error {status}); pacing by the clock");
                        self.link = Link::Unavailable;
                        return false;
                    }
                    CVDisplayLinkSetOutputCallback(link, tick, ptr::null_mut());
                    link
                }
            };
            if CVDisplayLinkStart(link) != 0 {
                warn!("the display link will not start; pacing by the clock");
                self.link = Link::Unavailable;
                return false;
            }
            debug!("display link on");
            self.link = Link::On(link);
            true
        }
    }

    /// Nothing is moving: the display link can rest.
    pub fn stop(&mut self) {
        if let Link::On(link) = self.link {
            unsafe { CVDisplayLinkStop(link) };
            self.link = Link::Off(link);
        }
    }

    /// Waits for the next refresh, or returns at once if one went by since
    /// the last wait. `Err` hands the wait back: there is no display link.
    pub fn wait(&mut self, timeout: Duration) -> Result<(), ()> {
        if !self.start() {
            return Err(());
        }
        let Ok(ticks) = TICKS.lock() else {
            return Err(());
        };
        let seen = self.seen;
        let (ticks, _) = TICKED
            .wait_timeout_while(ticks, timeout, |t| *t == seen)
            .map_err(|_| ())?;
        self.seen = *ticks;
        Ok(())
    }
}
