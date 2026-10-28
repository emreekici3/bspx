//! Runs the daemon against a fake window system with a real `bspxrc`,
//! which configures it through the real `bspx` binary and socket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bspx::bsp::WinId;
use bspx::geometry::Rect;
use bspx::platform::{Backend, WindowInfo};

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<WindowInfo>>>);

impl Backend for Fake {
    fn windows(&mut self) -> Vec<WindowInfo> {
        self.0.lock().unwrap().clone()
    }
    fn focused_window(&mut self) -> Option<WinId> {
        self.0.lock().unwrap().last().map(|w| w.id)
    }
    fn tiling_area(&mut self) -> Rect {
        Rect::new(0, 0, 1000, 600)
    }
    fn set_frame(&mut self, id: WinId, frame: Rect) {
        if let Some(w) = self.0.lock().unwrap().iter_mut().find(|w| w.id == id) {
            w.frame = frame;
        }
    }
    fn focus(&mut self, _id: WinId) {}
}

#[test]
fn bspxrc_configures_the_daemon() {
    let dir = std::env::temp_dir().join(format!("bspx-rc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("s.sock");
    let rc = dir.join("bspxrc");
    std::fs::write(
        &rc,
        "bspx config gap 20\nbspx config padding 0\nbspx config animation 0\nbspx rule float Notes\n",
    )
    .unwrap();

    // The script finds `bspx` on PATH and the socket through the environment.
    let bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_bspx"))
        .parent()
        .unwrap()
        .to_owned();
    let path = std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("BSPX_SOCKET", &socket);
    }

    let fake = Fake::default();
    {
        let mut w = fake.0.lock().unwrap();
        for (id, app) in [(1, "Terminal"), (2, "Safari"), (3, "Notes")] {
            w.push(WindowInfo {
                id,
                frame: Rect::new(50, 50, 200, 200),
                app: app.into(),
                tileable: true,
            });
        }
    }
    let server = bspx::ipc::Server::bind(&socket).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (fake, stop) = (fake.clone(), stop.clone());
        std::thread::spawn(move || bspx::daemon::run(fake, Some(rc), server, stop))
    };

    let frame = |id: WinId| {
        fake.0
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.id == id)
            .unwrap()
            .frame
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while frame(2) != Rect::new(510, 0, 490, 600) {
        assert!(
            Instant::now() < deadline,
            "layout never applied: {:?}",
            frame(2)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(frame(1), Rect::new(0, 0, 490, 600));
    assert_eq!(frame(3), Rect::new(50, 50, 200, 200), "Notes is floating");

    // Live change from the command line, like a user in a terminal.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bspx"))
        .args(["config", "gap"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "20\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bspx"))
        .args(["config", "gap", "oops"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a valid number"));

    stop.store(true, Ordering::Relaxed);
    daemon.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
