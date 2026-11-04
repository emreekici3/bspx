//! `cberry doctor`: looks at what cranberry needs and says what to do about
//! whatever is missing.

use crate::config::Config;
use crate::{ipc, service};

/// What was found on this Mac.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The config script, if there is one.
    pub config: Option<String>,
    /// The binary the login service runs, and whether it is still there.
    pub service: Option<(String, bool)>,
    /// The running window manager's version, or `None` if none answers. One
    /// too old to say gives `Some("older")`.
    pub running: Option<String>,
    /// The Spaces whose *Switch to Desktop N* shortcut is off.
    pub shortcuts_off: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    /// What was found, when all is well.
    pub found: Option<String>,
    /// What is wrong and what to do about it.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, found: impl Into<String>) -> Self {
        let found = Some(found.into());
        Self {
            name,
            found,
            fix: None,
        }
    }

    fn bad(name: &'static str, fix: impl Into<String>) -> Self {
        let fix = Some(fix.into());
        Self {
            name,
            found: None,
            fix,
        }
    }
}

/// What `facts` come to, for this `version` of cberry.
pub fn checks(facts: &Facts, version: &str) -> Vec<Check> {
    let config = match &facts.config {
        Some(path) => Check::ok("Config", path),
        None => Check::bad(
            "Config",
            "no config script: copy examples/cberryrc to ~/.config/cranberry/cberryrc",
        ),
    };
    let service = match &facts.service {
        Some((path, true)) => Check::ok("Service", path),
        Some((path, false)) => Check::bad(
            "Service",
            format!("{path} is gone: run `cberry install`, then `cberry restart`"),
        ),
        None => Check::bad("Service", "not installed: run `cberry install`"),
    };
    let running = match facts.running.as_deref() {
        Some(v) if v == version => Check::ok("Running", v),
        Some(v) => Check::bad(
            "Running",
            format!("version {v} is running, not {version}: run `cberry restart`"),
        ),
        None => Check::bad(
            "Running",
            "nothing answers: run `cberry start`, and enable cberry under System Settings > \
             Privacy & Security > Accessibility",
        ),
    };
    let shortcuts = if facts.running.is_none() {
        Check::ok("Shortcuts", "not checked: cranberry is not running")
    } else if facts.shortcuts_off.is_empty() {
        Check::ok("Shortcuts", "Switch to Desktop is on for every Space")
    } else {
        let spaces: Vec<String> = facts
            .shortcuts_off
            .iter()
            .map(ToString::to_string)
            .collect();
        Check::bad(
            "Shortcuts",
            format!(
                "Switch to Desktop {} is off: turn it on under System Settings > Keyboard > \
                 Keyboard Shortcuts > Mission Control",
                spaces.join(", ")
            ),
        )
    };
    vec![config, service, running, shortcuts]
}

/// The checks as lines to print.
pub fn report(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let (mark, text) = match (&check.fix, &check.found) {
            (Some(fix), _) => ("✗", fix.as_str()),
            (None, found) => ("✓", found.as_deref().unwrap_or("")),
        };
        out.push_str(&format!("{mark} {:<10}{text}\n", check.name));
    }
    out
}

/// How many Spaces this display has, from a `label` listing.
fn space_count(listing: &str) -> usize {
    listing.lines().count()
}

/// Looks at this Mac.
pub fn facts() -> Facts {
    let socket = ipc::socket_path();
    let daemon = |line: &str| ipc::send(&socket, line).ok();
    let running = daemon("daemon-version")
        .map(|reply| reply.map_or_else(|_| "older".to_string(), |v| v.trim().to_string()));
    let spaces = daemon("label")
        .and_then(Result::ok)
        .map_or(0, |listing| space_count(&listing));
    Facts {
        config: Config::find_rc().map(|p| p.display().to_string()),
        service: service::binary().map(|p| {
            let there = p.exists();
            (p.display().to_string(), there)
        }),
        running,
        shortcuts_off: (1..=spaces).filter(|n| !shortcut_on(*n)).collect(),
    }
}

#[cfg(any(target_os = "macos", cberry_check_macos))]
fn shortcut_on(n: usize) -> bool {
    crate::platform::macos::desktop_shortcut_on(n)
}

#[cfg(not(any(target_os = "macos", cberry_check_macos)))]
fn shortcut_on(_n: usize) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            config: Some("/Users/me/.config/cranberry/cberryrc".into()),
            service: Some(("/opt/homebrew/opt/cranberry/bin/cberry".into(), true)),
            running: Some("1.0.0".into()),
            shortcuts_off: Vec::new(),
        }
    }

    fn fixes(facts: &Facts) -> Vec<String> {
        checks(facts, "1.0.0")
            .into_iter()
            .filter_map(|c| c.fix)
            .collect()
    }

    #[test]
    fn a_healthy_mac_has_nothing_to_fix() {
        assert!(fixes(&healthy()).is_empty());
    }

    #[test]
    fn a_missing_config_says_where_it_goes() {
        let facts = Facts {
            config: None,
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("cberryrc"));
    }

    #[test]
    fn a_service_left_pointing_at_an_old_version_is_reinstalled() {
        let facts = Facts {
            service: Some((
                "/opt/homebrew/Cellar/cranberry/0.2.1/bin/cberry".into(),
                false,
            )),
            ..healthy()
        };
        let fix = &fixes(&facts)[0];
        assert!(
            fix.contains("is gone") && fix.contains("cberry install"),
            "{fix}"
        );
    }

    #[test]
    fn no_service_says_to_install_it() {
        let facts = Facts {
            service: None,
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("cberry install"));
    }

    #[test]
    fn nothing_running_mentions_accessibility() {
        let facts = Facts {
            running: None,
            shortcuts_off: vec![1],
            ..healthy()
        };
        let found = fixes(&facts);
        assert_eq!(
            found.len(),
            1,
            "shortcuts cannot be counted then: {found:?}"
        );
        assert!(found[0].contains("Accessibility"));
    }

    #[test]
    fn an_old_version_still_running_is_restarted() {
        let facts = Facts {
            running: Some("0.9.0".into()),
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("cberry restart"));
    }

    #[test]
    fn shortcuts_that_are_off_are_named() {
        let facts = Facts {
            shortcuts_off: vec![2, 3],
            ..healthy()
        };
        assert!(fixes(&facts)[0].contains("Switch to Desktop 2, 3"));
    }

    #[test]
    fn the_report_marks_each_check() {
        let facts = Facts {
            config: None,
            ..healthy()
        };
        let report = report(&checks(&facts, "1.0.0"));
        assert!(report.lines().next().unwrap().starts_with("✗ Config"));
        assert!(report.lines().nth(1).unwrap().starts_with("✓ Service"));
    }

    #[test]
    fn spaces_are_counted_from_the_label_listing() {
        assert_eq!(space_count("1\tgeneral\tcurrent\n2\t-\n3\tweb\n"), 3);
        assert_eq!(space_count(""), 0);
    }
}
