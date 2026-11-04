//! Running cberry as a launchd user agent: `cberry install`, `start`, `stop`,
//! `restart`, `uninstall`. launchd starts it at login and in the background,
//! so no terminal has to stay open.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.github.emreekici3.cranberry";

/// Seconds launchd waits before starting cberry again after a crash.
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
        dir.join(format!("cberry_{user}.out.log")),
        dir.join(format!("cberry_{user}.err.log")),
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent for `exe`. `path` is the `PATH` the service runs with,
/// so `cberryrc` can find `cberry` and anything else it calls.
/// The binary a service plist runs.
fn binary_in_plist(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once("<key>ProgramArguments</key>")?;
    let (_, rest) = rest.split_once("<string>")?;
    rest.split_once("</string>").map(|(path, _)| path)
}

/// The binary the installed service runs, if one is installed.
pub fn binary() -> Option<PathBuf> {
    let text = std::fs::read_to_string(plist_path().ok()?).ok()?;
    let path = binary_in_plist(&text)?
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    Some(PathBuf::from(path))
}

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

/// The path of `exe` that outlives an upgrade. Homebrew keeps each version in
/// a folder of its own and removes the old one, but `opt` always points at the
/// current version: `<prefix>/Cellar/<name>/<version>/bin/cberry` is
/// `<prefix>/opt/<name>/bin/cberry`.
fn stable_path(exe: &Path) -> PathBuf {
    let parts: Vec<_> = exe.components().collect();
    let Some(cellar) = parts.iter().position(|c| c.as_os_str() == "Cellar") else {
        return exe.to_path_buf();
    };
    if parts.len() < cellar + 4 {
        return exe.to_path_buf();
    }
    let mut path: PathBuf = parts[..cellar].iter().collect();
    path.push("opt");
    path.push(parts[cellar + 1]);
    path.extend(&parts[cellar + 3..]);
    path
}

pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot find the cberry binary: {e}"))?;
    let exe = stable_path(&exe);
    let plist_file = plist_path()?;
    if let Some(dir) = plist_file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let (out, err) = log_paths();
    std::fs::write(&plist_file, plist(&exe, &service_path(&exe), &out, &err))
        .map_err(|e| format!("{}: {e}", plist_file.display()))?;
    Ok(format!(
        "installed {}\nrun `cberry start` to start it; it will also start at login.",
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("cberry was not installed".into()),
        Err(e) => Err(format!("{}: {e}", plist_file.display())),
    }
}

pub fn start() -> Result<String, String> {
    let plist_file = plist_path()?;
    if !plist_file.is_file() {
        return Err(
            "cberry is not installed as a service; run `cberry install` first \
                    (or `cberry run` to run it in this terminal)"
                .into(),
        );
    }
    if is_loaded() {
        launchctl(&["kickstart", &service_target()])?;
    } else {
        launchctl(&["bootstrap", &domain(), &plist_file.to_string_lossy()])?;
    }
    let (_, err) = log_paths();
    Ok(format!("cberry started (log: {})", err.display()))
}

pub fn stop() -> Result<String, String> {
    if !is_loaded() {
        return Ok("cberry is not running as a service".into());
    }
    launchctl(&["bootout", &service_target()])?;
    Ok("cberry stopped".into())
}

pub fn restart() -> Result<String, String> {
    if !is_loaded() {
        return start();
    }
    launchctl(&["kickstart", "-k", &service_target()])?;
    Ok("cberry restarted".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_binary_in_the_foreground() {
        let p = plist(
            Path::new("/Users/me/.cargo/bin/cberry"),
            "/Users/me/.cargo/bin:/usr/bin",
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
        );
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
        assert!(p.contains(
            "<string>/Users/me/.cargo/bin/cberry</string>\n        <string>run</string>"
        ));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<string>/Users/me/.cargo/bin:/usr/bin</string>"));
        assert!(p.contains("<string>/tmp/err</string>"));
    }

    #[test]
    fn plist_escapes_paths() {
        let p = plist(
            Path::new("/a&b/<cberry>"),
            "x",
            Path::new("o"),
            Path::new("e"),
        );
        assert!(p.contains("/a&amp;b/&lt;cberry&gt;"));
    }

    #[test]
    fn a_homebrew_binary_is_run_from_the_path_that_outlives_upgrades() {
        assert_eq!(
            stable_path(Path::new("/opt/homebrew/Cellar/cranberry/0.2.2/bin/cberry")),
            Path::new("/opt/homebrew/opt/cranberry/bin/cberry")
        );
        assert_eq!(
            stable_path(Path::new("/usr/local/Cellar/cranberry/0.2.2_1/bin/cberry")),
            Path::new("/usr/local/opt/cranberry/bin/cberry")
        );
    }

    #[test]
    fn the_binary_is_read_back_from_the_plist() {
        let text = plist(
            Path::new("/a/cberry"),
            "/usr/bin",
            Path::new("/o"),
            Path::new("/e"),
        );
        assert_eq!(binary_in_plist(&text), Some("/a/cberry"));
        assert_eq!(binary_in_plist("<plist/>"), None);
    }

    #[test]
    fn any_other_binary_stays_where_it_is() {
        for path in ["/Users/me/bin/cberry", "/usr/local/bin/cberry"] {
            assert_eq!(stable_path(Path::new(path)), Path::new(path));
        }
    }

    #[test]
    fn service_path_puts_the_binary_first() {
        let path = service_path(Path::new("/Users/me/.cargo/bin/cberry"));
        assert!(path.starts_with("/Users/me/.cargo/bin:"));
        assert!(path.contains("/opt/homebrew/bin"));
        let path = service_path(Path::new("/usr/local/bin/cberry"));
        assert_eq!(path.matches("/usr/local/bin").count(), 1);
    }
}
