//! Dependency regression guard, not a substitute for a real Touch Bar quit/close test.
//!
//! AccessKit #835 preserves AppKit's KVO subclass when its adapter is dropped. The old
//! 0.26.3 resolution aborted during native window teardown, after our shutdown hook.

use semver::Version;

/// Require every locked macOS adapter to be a stable release containing the KVO teardown fix.
fn check_lock(lock: &str) -> Result<(), String> {
    let lock: toml::Value = toml::from_str(lock).map_err(|e| e.to_string())?;
    let packages = lock.get("package").and_then(toml::Value::as_array).ok_or("missing package list")?;
    let mut found = false;
    for package in packages.iter().filter(|p| p.get("name").and_then(toml::Value::as_str) == Some("accesskit_macos")) {
        found = true;
        let text = package.get("version").and_then(toml::Value::as_str).ok_or("missing version")?;
        let version = Version::parse(text).map_err(|e| e.to_string())?;
        if version < Version::new(0, 27, 2) || !version.pre.is_empty() {
            return Err(format!("accesskit_macos {version} lacks the required stable KVO fix"));
        }
    }
    if found { Ok(()) } else { Err("accesskit_macos is missing".into()) }
}

#[test]
fn locked_macos_adapter_contains_the_kvo_fix() {
    assert_eq!(check_lock(include_str!("../../../Cargo.lock")), Ok(()));
}

#[test]
fn guard_rejects_the_crashing_resolution_and_uncertain_inputs() {
    for version in ["0.26.3", "0.27.1", "0.27.2-alpha.1", "0.28.0-beta.1", "invalid"] {
        let lock = format!("[[package]]\nname = 'accesskit_macos'\nversion = '{version}'\n");
        assert!(check_lock(&lock).is_err(), "accepted {version}");
    }
    for lock in ["", "package = []", "[[package]]\nname = 'accesskit_macos'", "not valid toml"] {
        assert!(check_lock(lock).is_err());
    }
}

#[test]
fn guard_accepts_fixed_releases_but_checks_every_resolution() {
    for version in ["0.27.2", "0.27.10", "0.28.0", "1.0.0"] {
        let lock = format!("[[package]]\nname = 'accesskit_macos'\nversion = '{version}'\n");
        assert_eq!(check_lock(&lock), Ok(()));
    }
    let mixed = "[[package]]\nname = 'accesskit_macos'\nversion = '0.27.2'\n\
                 [[package]]\nname = 'accesskit_macos'\nversion = '0.26.3'\n";
    assert!(check_lock(mixed).is_err());
}
