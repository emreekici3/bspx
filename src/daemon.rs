//! The daemon's main loop.

use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use log::{error, info, warn};

use crate::command::Command;
use crate::config::Config;
use crate::ipc::Server;
use crate::manager::Manager;
use crate::platform::Backend;

/// Frame interval while something is moving (120 fps; ProMotion friendly).
const ACTIVE_FRAME: Duration = Duration::from_micros(8_333);
/// Loop interval when nothing is moving.
const IDLE_FRAME: Duration = Duration::from_millis(16);
/// How often to look again while the backend waits for a new window or
/// app to settle.
const RETRY: Duration = Duration::from_millis(100);
/// One more look this long after the last event: right after a Space switch
/// or an app launch the window list can still show how things were.
const FOLLOW_UP: Duration = Duration::from_millis(300);
/// How long to hold off tiling at startup while the config script runs, so
/// windows are laid out once with the user's settings instead of jumping
/// from the defaults.
const RC_GRACE: Duration = Duration::from_secs(3);

/// A running `bspxrc`.
struct RcScript {
    child: Child,
    started: Instant,
}

impl RcScript {
    /// Starts `sh <path>` in the background. The script talks back to the
    /// daemon through `bspx config ...`, so `bspx` must be on its `PATH`.
    fn spawn(path: &Path) -> Option<Self> {
        let mut cmd = Process::new("/bin/sh");
        cmd.arg(path);
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_owned))
        {
            let path_var = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs = vec![dir];
            dirs.extend(std::env::split_paths(&path_var));
            if let Ok(joined) = std::env::join_paths(dirs) {
                cmd.env("PATH", joined);
            }
        }
        match cmd.spawn() {
            Ok(child) => {
                info!("running {}", path.display());
                Some(Self {
                    child,
                    started: Instant::now(),
                })
            }
            Err(e) => {
                warn!("cannot run {}: {e}", path.display());
                None
            }
        }
    }

    /// Whether the script is still running (and within the startup grace period).
    fn pending(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    warn!("config script exited with {status}");
                }
                false
            }
            Ok(None) => self.started.elapsed() < RC_GRACE,
            Err(_) => false,
        }
    }
}

impl Drop for RcScript {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Runs until a `quit` command arrives or `stop` is set.
///
/// There is no polling: the window list is read again only when something
/// says it changed (a window or app event, a command), once more shortly
/// after the last event, when an animation ends (to see where the windows
/// really went), and, briefly, while the backend waits for a new window or
/// app to settle.
pub fn run<B: Backend>(
    backend: B,
    rc_path: Option<PathBuf>,
    server: Server,
    stop: Arc<AtomicBool>,
) {
    let mut manager = Manager::new(backend, Config::default());

    let mut rc = rc_path.as_deref().and_then(RcScript::spawn);
    if rc.is_none() {
        info!("no config script found, using defaults");
    }
    let mut started = false;

    let mut last_frame = Instant::now();
    let mut last_look = Instant::now();
    let mut was_animating = false;
    let mut follow_up: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        let frame_start = Instant::now();
        let mut reload = false;
        let mut commands = false;
        let mut handle = |manager: &mut Manager<B>, line: &str| {
            commands = true;
            let cmd = Command::parse(line)?;
            match cmd {
                Command::Quit => {
                    stop.store(true, Ordering::Relaxed);
                    Ok("bye".into())
                }
                Command::Reload => {
                    reload = true;
                    Ok("reloading".into())
                }
                cmd => manager.execute(&cmd),
            }
        };
        server.poll(|line| handle(&mut manager, line));
        // Keyboard shortcuts whose command is `bspx ...` run here, at once.
        for line in manager.hotkey_commands() {
            if let Err(e) = handle(&mut manager, &line) {
                warn!("hotkey 'bspx {line}': {e}");
            }
        }
        if reload {
            // Start from the defaults so removed lines take effect too.
            manager.set_config(Config::default());
            let path = Config::find_rc();
            rc = path.as_deref().and_then(RcScript::spawn);
            info!("config reloaded");
        }

        let rc_running = rc.as_mut().is_some_and(RcScript::pending);
        if !rc_running {
            rc = None;
        }
        let mut look = false;
        if !started && !rc_running {
            started = true;
            look = true;
        }
        // A command may have changed a rule or a setting the layout must
        // follow (what a poll used to pick up).
        look |= commands;

        let now = Instant::now();
        let dt = now.duration_since(last_frame).as_secs_f64();
        last_frame = now;
        manager.tick(dt);
        if started {
            manager.track_pointer();
        }
        manager.pump();
        if manager.handle_events() {
            look = true;
            follow_up = Some(Instant::now() + FOLLOW_UP);
        }
        if follow_up.is_some_and(|at| Instant::now() >= at) {
            follow_up = None;
            look = true;
        }

        // Where the windows really went, once they stop: apps that refused
        // a size, a window that does not fit after all.
        let animating = manager.is_animating();
        look |= was_animating && !animating;
        was_animating = animating;

        look |= last_look.elapsed() >= RETRY && manager.wants_another_look();

        if started && look {
            last_look = Instant::now();
            manager.refresh();
        }

        // Sleep only what is left of the frame, so frames stay evenly spaced
        // however long this one's work (mostly Accessibility calls) took.
        let interval = if manager.is_animating() {
            ACTIVE_FRAME
        } else {
            IDLE_FRAME
        };
        std::thread::sleep(interval.saturating_sub(frame_start.elapsed()));
    }
    info!("stopping");
}

/// Logs a fatal startup error and exits.
pub fn fail(msg: impl std::fmt::Display) -> ! {
    error!("{msg}");
    eprintln!("bspx: {msg}");
    std::process::exit(1);
}
