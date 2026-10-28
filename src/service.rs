//! Running bspx as a launchd user agent: `bspx install`, `start`, `stop`,
//! `restart`, `uninstall`. launchd starts it at login and in the background,
//! so no terminal has to stay open.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.github.emreekici3.bspx";

/// Seconds launchd waits before starting bspx again after a crash.
const THROTTLE_SECS: u32 = 10;

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

pub fn plist_path() -> Result<PathBuf, String> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// Where the service's output goes.
pub fn log_paths() -> (PathBuf, PathBuf) {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let dir = std::env::temp_dir();
    (
        dir.join(format!("bspx_{user}.out.log")),
        dir.join(format!("bspx_{user}.err.log")),
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent for `exe`. `path` is the `PATH` the service runs with,
/// so `bspxrc` can find `bspx` and anything else it calls.
pub fn plist(exe: &Path, path: &str, stdout: &Path, stderr: &Path) -> String {
    let exe = escape(&exe.to_string_lossy());
    let path = escape(path);
    let stdout = escape(&stdout.to_string_lossy());
    let stderr = escape(&stderr.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>{THROTTLE_SECS}</integer>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#
    )
}

/// The `PATH` for the service: the directory of `exe` first, then the
/// usual system and Homebrew locations.
pub fn service_path(exe: &Path) -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Some(dir) = exe.parent() {
        dirs.push(dir.to_string_lossy().into_owned());
    }
    for d in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        if !dirs.iter().any(|x| x == d) {
            dirs.push(d.to_string());
        }
    }
    dirs.join(":")
}

unsafe extern "C" {
    fn getuid() -> u32;
}

fn domain() -> String {
    format!("gui/{}", unsafe { getuid() })
}

fn service_target() -> String {
    format!("{}/{LABEL}", domain())
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!("launchctl {} failed: {msg}", args.join(" ")))
    }
}

fn is_loaded() -> bool {
    Command::new("launchctl")
        .args(["print", &service_target()])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot find the bspx binary: {e}"))?;
    let plist_file = plist_path()?;
    if let Some(dir) = plist_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let (out, err) = log_paths();
    std::fs::write(&plist_file, plist(&exe, &service_path(&exe), &out, &err))
        .map_err(|e| format!("{}: {e}", plist_file.display()))?;
    Ok(format!(
        "installed {}\nrun `bspx start` to start it; it will also start at login.",
        plist_file.display()
    ))
}

pub fn uninstall() -> Result<String, String> {
    let plist_file = plist_path()?;
    if is_loaded() {
        launchctl(&["bootout", &service_target()])?;
    }
    match std::fs::remove_file(&plist_file) {
        Ok(()) => Ok(format!("removed {}", plist_file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("bspx was not installed".into()),
        Err(e) => Err(format!("{}: {e}", plist_file.display())),
    }
}

pub fn start() -> Result<String, String> {
    let plist_file = plist_path()?;
    if !plist_file.is_file() {
        return Err(
            "bspx is not installed as a service; run `bspx install` first \
                    (or `bspx run` to run it in this terminal)"
                .into(),
        );
    }
    if is_loaded() {
        launchctl(&["kickstart", &service_target()])?;
    } else {
        launchctl(&["bootstrap", &domain(), &plist_file.to_string_lossy()])?;
    }
    let (_, err) = log_paths();
    Ok(format!("bspx started (log: {})", err.display()))
}

pub fn stop() -> Result<String, String> {
    if !is_loaded() {
        return Ok("bspx is not running as a service".into());
    }
    launchctl(&["bootout", &service_target()])?;
    Ok("bspx stopped".into())
}

pub fn restart() -> Result<String, String> {
    if !is_loaded() {
        return start();
    }
    launchctl(&["kickstart", "-k", &service_target()])?;
    Ok("bspx restarted".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_binary_in_the_foreground() {
        let p = plist(
            Path::new("/Users/me/.cargo/bin/bspx"),
            "/Users/me/.cargo/bin:/usr/bin",
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
        );
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
        assert!(
            p.contains("<string>/Users/me/.cargo/bin/bspx</string>\n        <string>run</string>")
        );
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<string>/Users/me/.cargo/bin:/usr/bin</string>"));
        assert!(p.contains("<string>/tmp/err</string>"));
    }

    #[test]
    fn plist_escapes_paths() {
        let p = plist(
            Path::new("/a&b/<bspx>"),
            "x",
            Path::new("o"),
            Path::new("e"),
        );
        assert!(p.contains("/a&amp;b/&lt;bspx&gt;"));
    }

    #[test]
    fn service_path_puts_the_binary_first() {
        let path = service_path(Path::new("/Users/me/.cargo/bin/bspx"));
        assert!(path.starts_with("/Users/me/.cargo/bin:"));
        assert!(path.contains("/opt/homebrew/bin"));
        let path = service_path(Path::new("/usr/local/bin/bspx"));
        assert_eq!(path.matches("/usr/local/bin").count(), 1);
    }
}
