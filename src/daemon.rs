//! The daemon's main loop.

use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};

use crate::command::Command;
use crate::config::Config;
use crate::ipc::Server;
use crate::manager::Manager;
use crate::motion::paced;
use crate::platform::Backend;

/// Loop interval when nothing is moving. While something is, the loop
/// runs at the display's refresh rate (`Manager::frame_interval`).
const IDLE_FRAME: Duration = Duration::from_millis(16);
/// Work that takes longer than this is logged (at debug level), to find
/// what keeps the layout waiting.
const SLOW: Duration = Duration::from_millis(40);

/// How often to look again while the backend waits for a new window or
/// app to settle.
const RETRY: Duration = Duration::from_millis(100);
/// For this long after the last event, look again every `RETRY`. Right
/// after an event the window list can still show how things were: a closed
/// window stays listed while it fades out (and some apps, Terminal among
/// them, report the close a second or more later), the old Space shows for
/// a moment after a switch, a new app's windows come up a little later.
const SETTLE: Duration = Duration::from_secs(1);
/// How long to hold off tiling at startup while the config script runs, so
/// windows are laid out once with the user's settings instead of jumping
/// from the defaults.
const RC_GRACE: Duration = Duration::from_secs(3);

/// A running `cberryrc`.
struct RcScript {
    child: Child,
    started: Instant,
}

impl RcScript {
    /// Starts `sh <path>` in the background. The script talks back to the
    /// daemon through `cberry config ...`, so `cberry` must be on its `PATH`.
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
/// says it changed (a window or app event, a command), every tenth of a
/// second for a second after the last event, when an animation ends (to see
/// where the windows really went), and, briefly, while the backend waits
/// for a new window or app to settle. Left alone, cberry does not look at all.
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
    // Until when to keep looking after the last event.
    let mut settle_until: Option<Instant> = None;
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
        // Keyboard shortcuts whose command is `cberry ...` run here, at once.
        for line in manager.hotkey_commands() {
            if let Err(e) = handle(&mut manager, &line) {
                warn!("hotkey 'cberry {line}': {e}");
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
        let refresh = manager.frame_interval().as_secs_f64();
        let dt = paced(dt, refresh);
        last_frame = now;
        manager.tick(dt);
        let took = now.elapsed();
        if took > SLOW {
            debug!("slow animation frame: {} ms", took.as_millis());
        }
        if started {
            manager.track_pointer();
        }
        manager.pump();
        if manager.handle_events() {
            look = true;
            settle_until = Some(Instant::now() + SETTLE);
        }
        if settle_until.is_some_and(|until| Instant::now() >= until) {
            settle_until = None;
        }
        look |= settle_until.is_some() && last_look.elapsed() >= RETRY;

        // Where the windows really went, once they stop: apps that refused
        // a size, a window that does not fit after all.
        let animating = manager.is_animating() || manager.is_held();
        look |= was_animating && !animating;
        was_animating = animating;

        look |= last_look.elapsed() >= RETRY && manager.wants_another_look();

        if started && look {
            last_look = Instant::now();
            manager.refresh();
            let took = last_look.elapsed();
            if took > SLOW {
                debug!("slow look at the window list: {} ms", took.as_millis());
            }
        }

        // Sleep only what is left of the frame, so frames stay evenly spaced
        // however long this one's work took. The windows are written by
        // their apps' own threads, so a slow app does not make it late.
        if manager.is_animating() || manager.is_held() {
            let remaining = manager
                .frame_interval()
                .saturating_sub(frame_start.elapsed());
            manager.wait_frame(remaining);
        } else {
            manager.stop_frames();
            std::thread::sleep(IDLE_FRAME.saturating_sub(frame_start.elapsed()));
        }
    }
    info!("stopping");
}

/// Logs a fatal startup error and exits.
pub fn fail(msg: impl std::fmt::Display) -> ! {
    error!("{msg}");
    eprintln!("cberry: {msg}");
    std::process::exit(1);
}
