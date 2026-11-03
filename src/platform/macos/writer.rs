//! Frame writes, one thread per app.
//!
//! Every frame of an animation is written to the window through two
//! Accessibility calls, and each call waits for the app: Safari can take
//! 35 ms to lay itself out again. Made one after the other on the main
//! loop, these calls held every window back to the slowest app's pace.
//!
//! So the main loop only leaves the latest frame of each window in its
//! app's mailbox, and the app's own thread does the writing. A slow app
//! skips the frames it cannot keep up with (they never pile up: only the
//! latest one waits), and the others move at their own pace.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, warn};

use super::ffi::*;
use super::{Cf, Step, cfstring, read_frame, set_attr};
use crate::bsp::WinId;
use crate::geometry::Rect;

/// A write that takes longer than this is logged (at debug level).
const SLOW_WRITE: Duration = Duration::from_millis(40);

/// One window's frame, to be written in the order of `steps`.
pub struct Job {
    pub id: WinId,
    pub element: Cf,
    pub frame: Rect,
    pub steps: Vec<Step>,
    /// The frame the window stays at: the last of an animation, or a frame
    /// set at once. Looking at the window list waits for these.
    pub last: bool,
    /// Log where the app put the window after each call.
    pub trace: bool,
}

// A `Job` holds a retained AX element. CoreFoundation's reference counting
// is thread-safe, and Accessibility calls may be made from any thread.
unsafe impl Send for Job {}

#[derive(Default)]
struct State {
    /// The latest job of each window, in the order the windows were sent.
    jobs: Vec<Job>,
    /// The thread is writing a batch with a last frame in it.
    writing_last: bool,
    stop: bool,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<State>,
    /// Jobs arrived, or stop.
    wake: Condvar,
    /// A batch was written.
    done: Condvar,
}

impl Mailbox {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A writer thread never panics while holding the lock; if one did,
        // its jobs are still fine to read.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The writer threads, one per app that has had a frame written.
#[derive(Default)]
pub struct Writers {
    apps: HashMap<i32, Arc<Mailbox>>,
}

impl Writers {
    /// Leaves `job` for app `pid`'s thread, replacing the job of the same
    /// window that has not been written yet.
    pub fn send(&mut self, pid: i32, job: Job) {
        let mailbox = match self.apps.entry(pid) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => match spawn(pid) {
                Some(mailbox) => e.insert(mailbox),
                None => {
                    // No thread: write here, as before.
                    write(&job, &cfstring("AXPosition"), &cfstring("AXSize"));
                    return;
                }
            },
        };
        let mut state = mailbox.lock();
        match state.jobs.iter_mut().find(|j| j.id == job.id) {
            Some(slot) => {
                // A last frame stays a last frame until it is written.
                let last = slot.last || job.last;
                *slot = job;
                slot.last = last;
            }
            None => state.jobs.push(job),
        }
        mailbox.wake.notify_one();
    }

    /// Stops the threads of apps that are gone.
    pub fn retain(&mut self, alive: impl Fn(i32) -> bool) {
        self.apps.retain(|pid, mailbox| {
            let keep = alive(*pid);
            if !keep {
                mailbox.lock().stop = true;
                mailbox.wake.notify_one();
            }
            keep
        });
    }

    /// Waits, up to `timeout`, until every last frame sent has been
    /// written, so that reading the window list sees windows where they
    /// were left rather than a frame or two before.
    pub fn wait_for_last_frames(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        for mailbox in self.apps.values() {
            let mut state = mailbox.lock();
            while state.writing_last || state.jobs.iter().any(|j| j.last) {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return;
                };
                state = mailbox
                    .done
                    .wait_timeout(state, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
    }
}

fn spawn(pid: i32) -> Option<Arc<Mailbox>> {
    let mailbox = Arc::new(Mailbox::default());
    let theirs = Arc::clone(&mailbox);
    let spawned = thread::Builder::new()
        .name(format!("cberry-writer-{pid}"))
        .spawn(move || run(&theirs));
    match spawned {
        Ok(_) => Some(mailbox),
        Err(e) => {
            warn!("no writer thread for pid {pid}: {e}");
            None
        }
    }
}

fn run(mailbox: &Mailbox) {
    let position = cfstring("AXPosition");
    let size = cfstring("AXSize");
    loop {
        let jobs = {
            let mut state = mailbox.lock();
            while state.jobs.is_empty() && !state.stop {
                state = mailbox.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.stop {
                return;
            }
            state.writing_last = state.jobs.iter().any(|j| j.last);
            std::mem::take(&mut state.jobs)
        };
        for job in &jobs {
            write(job, &position, &size);
        }
        drop(jobs);
        mailbox.lock().writing_last = false;
        mailbox.done.notify_all();
    }
}

/// Writes one job: one AX call per step, so the window spends a moment at
/// every step in between.
fn write(job: &Job, position: &Cf, size: &Cf) {
    let Some(&first) = job.steps.first() else {
        return;
    };
    let element = job.element.ptr();
    let frame = job.frame;
    let point = CGPoint {
        x: f64::from(frame.x),
        y: f64::from(frame.y),
    };
    let dims = CGSize {
        width: f64::from(frame.w),
        height: f64::from(frame.h),
    };
    let one = |step: Step| unsafe {
        let value = match step {
            Step::Position => AXValueCreate(kAXValueTypeCGPoint, (&raw const point).cast()),
            Step::Size => AXValueCreate(kAXValueTypeCGSize, (&raw const dims).cast()),
        };
        if let Some(v) = Cf::owned(value) {
            let name = match step {
                Step::Position => position,
                Step::Size => size,
            };
            set_attr(element, name, v.ptr());
        }
    };
    let started = Instant::now();
    one(first);
    let mid = job
        .trace
        .then(|| read_frame(element, position, size))
        .flatten();
    for &step in &job.steps[1..] {
        one(step);
    }
    let took = started.elapsed();
    if took > SLOW_WRITE {
        debug!("slow write to window {}: {} ms", job.id, took.as_millis());
    }
    if job.trace {
        let edges = |r: Option<Rect>| {
            r.map_or_else(
                || "?".to_string(),
                |r| format!("{}..{} w{}", r.x, r.max_x(), r.w),
            )
        };
        let order: Vec<&str> = job
            .steps
            .iter()
            .map(|s| match s {
                Step::Position => "pos",
                Step::Size => "size",
            })
            .collect();
        debug!(
            target: "cranberry::frames",
            "window {} {}: want {} | between {} | after {}",
            job.id,
            order.join(","),
            edges(Some(frame)),
            edges(mid),
            edges(read_frame(element, position, size)),
        );
    }
}
