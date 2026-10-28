use std::process::ExitCode;

use bspx::command::{Command, USAGE};
use bspx::ipc;

fn print_help() {
    println!("bspx - animated BSP tiling window manager for macOS\n");
    println!("service:");
    println!("  bspx install            install bspx as a login service");
    println!("  bspx start              start the service");
    println!("  bspx stop               stop the service");
    println!("  bspx restart            restart the service");
    println!("  bspx uninstall          remove the service");
    println!("  bspx run                run in this terminal instead (for debugging)\n");
    println!("{USAGE}");
}

fn report(result: Result<String, String>) -> ExitCode {
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("bspx: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("run") => run(),
        // Internal: asks macOS in a fresh process (see `wait_for_accessibility`).
        Some("__check-accessibility") => check_accessibility(),
        Some(cmd @ ("install" | "uninstall" | "start" | "stop" | "restart")) => service(cmd),
        Some(_) => {
            let line = args.join(" ");
            if let Err(e) = Command::parse(&line) {
                eprintln!("bspx: {e}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            match ipc::send(&ipc::socket_path(), &line) {
                Ok(Ok(msg)) => {
                    print!("{msg}");
                    ExitCode::SUCCESS
                }
                Ok(Err(msg)) => {
                    eprintln!("bspx: {msg}");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!(
                        "bspx: cannot reach the window manager ({e}). Is it running? Try `bspx start`."
                    );
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", bspx_check_macos))]
fn service(cmd: &str) -> ExitCode {
    use bspx::service;
    report(match cmd {
        "install" => service::install(),
        "uninstall" => service::uninstall(),
        "start" => service::start(),
        "stop" => service::stop(),
        _ => service::restart(),
    })
}

#[cfg(not(any(target_os = "macos", bspx_check_macos)))]
fn service(_cmd: &str) -> ExitCode {
    report(Err("the service only runs on macOS".into()))
}

#[cfg(any(target_os = "macos", bspx_check_macos))]
fn run() -> ExitCode {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use bspx::config::Config;
    use bspx::daemon;
    use bspx::platform::macos::{MacBackend, ensure_accessibility};

    // Let everything up to debug through, but show only info unless asked:
    // `bspx config log_level debug` turns the rest on without a restart.
    let env = env_logger::Env::default();
    let chosen = std::env::var_os(env_logger::DEFAULT_FILTER_ENV).is_some();
    env_logger::Builder::from_env(env.default_filter_or("debug")).init();
    if !chosen {
        log::set_max_level(log::LevelFilter::Info);
    }

    // Ask once, then wait: exiting would make launchd start bspx again
    // and again, asking each time.
    if !ensure_accessibility(true) {
        wait_for_accessibility();
    }
    let rc = Config::find_rc();
    if let Some(p) = &rc {
        log::info!("config: {}", p.display());
    }
    let server = ipc::Server::bind(&ipc::socket_path()).unwrap_or_else(|e| daemon::fail(e));
    daemon::run(
        MacBackend::new(),
        rc,
        server,
        Arc::new(AtomicBool::new(false)),
    );
    ExitCode::SUCCESS
}

/// Blocks until bspx may use the Accessibility API, then starts over.
///
/// macOS does not tell a running process that it was granted access: asking
/// again from this process keeps saying no until it restarts. So a fresh
/// copy of bspx is asked every couple of seconds, and once it says yes this
/// process replaces itself with a new `bspx run`.
#[cfg(any(target_os = "macos", bspx_check_macos))]
fn wait_for_accessibility() {
    use std::os::unix::process::CommandExt;
    use std::time::Duration;

    log::warn!(
        "waiting for accessibility permission: allow bspx in System Settings > \
         Privacy & Security > Accessibility"
    );
    let exe = std::env::current_exe().unwrap_or_else(|e| bspx::daemon::fail(e));
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let granted = std::process::Command::new(&exe)
            .arg("__check-accessibility")
            .status()
            .is_ok_and(|s| s.success());
        if granted {
            log::info!("accessibility permission granted, starting");
            let err = std::process::Command::new(&exe).arg("run").exec();
            bspx::daemon::fail(format!("cannot restart bspx: {err}"));
        }
    }
}

#[cfg(any(target_os = "macos", bspx_check_macos))]
fn check_accessibility() -> ExitCode {
    if bspx::platform::macos::ensure_accessibility(false) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(not(any(target_os = "macos", bspx_check_macos)))]
fn check_accessibility() -> ExitCode {
    ExitCode::FAILURE
}

#[cfg(not(any(target_os = "macos", bspx_check_macos)))]
fn run() -> ExitCode {
    report(Err("the window manager only runs on macOS".into()))
}
