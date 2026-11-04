//! The device check for an Android app: the debug APK is installed on the build device, each
//! screen of the map is reached again by its keys, and claims judge it against the original's
//! screen (the focus, the controls by their text, their places in dp, no crash). Once the build
//! device is good enough and a second device is set, the same runs there as a gate.
//!
//! The judging is pure ([`state_claims`], [`likeness`], [`scorecard`], [`weakest`]); only
//! [`DeviceCheck::run`] touches a device.

use super::*;
use craft_anything_engine::workspace::Claim;
use serde::Serialize;

/// The check's id, and its folder under checks/round-N/.
pub const DEVICE: &str = "device";

/// Labels as compared: without case, extra spacing or a trailing colon.
fn key_of(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(':').trim().to_lowercase()
}

/// A screen's controls as (key, label shown, dp bounds) for the ones a person can land on
/// (focusable or clickable) that have words: their own title, else the titles inside them (a
/// Compose button is a wordless group with a text child). Each label once, in order.
fn landable(tree: &Value) -> Vec<(String, String, Option<[i64; 4]>)> {
    let nodes: Vec<&Value> = tree.get("nodes").and_then(Value::as_array).into_iter().flatten().collect();
    let depth = |n: &Value| n.get("depth").and_then(Value::as_u64).unwrap_or(0);
    let title = |n: &Value| n.get("title").and_then(Value::as_str).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if !(n["focusable"].as_bool() == Some(true) || n["clickable"].as_bool() == Some(true)) {
            continue;
        }
        let mut shown = title(n);
        if shown.is_empty() {
            let inside: Vec<String> = nodes.iter().skip(i + 1).take_while(|m| depth(m) > depth(n)).map(|m| title(m)).filter(|t| !t.is_empty()).collect();
            shown = inside.join(" ");
            if shown.len() > 80 {
                shown.truncate(shown.floor_char_boundary(80));
            }
        }
        let key = key_of(&shown);
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        let dp = n.get("dp").and_then(|d| Some([d["x"].as_i64()?, d["y"].as_i64()?, d["width"].as_i64()?, d["height"].as_i64()?]));
        out.push((key, shown, dp));
    }
    out
}

/// The claims for one screen: it was read; the same control has the focus; each control of the
/// original a person can land on is there by its text; each such pair sits within `tolerance`
/// dp. `original` and `built` are the two `ui.json` trees. An unreadable build screen is one
/// failed claim and nothing else: nothing can be said about it.
pub fn state_claims(id: &str, name: &str, original: &Value, built: &Value, tolerance_dp: u32) -> Vec<Claim> {
    let who = format!("{id} {name}");
    let built_count = built.get("count").and_then(Value::as_u64).unwrap_or(0);
    if built_count == 0 {
        return vec![Claim::new(
            format!("{who}: the screen is read on the device"),
            false,
            "the build's screen couldn't be read (no controls, or the app didn't reach it)",
        )];
    }
    let mut claims = vec![Claim::new(format!("{who}: the screen is read on the device"), true, "")];
    if let Some(focus) = original.get("focused_title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        let ours = built.get("focused_title").and_then(Value::as_str).unwrap_or("");
        let detail = if ours.trim().is_empty() { "nothing has the focus".to_string() } else { format!("the focus is on \"{ours}\"") };
        claims.push(Claim::new(format!("{who}: the focus is on \"{focus}\""), key_of(focus) == key_of(ours), detail));
    }
    let theirs = landable(original);
    let ours = landable(built);
    let tol = i64::from(tolerance_dp);
    for (key, shown, dp) in theirs.iter().take(40) {
        let found = ours.iter().find(|(k, ..)| k == key);
        claims.push(Claim::new(format!("{who}: it has \"{shown}\""), found.is_some(), "no control with that text"));
        if let (Some(a), Some((_, _, Some(b)))) = (dp, found) {
            let off = (0..4).map(|i| (a[i] - b[i]).abs()).max().unwrap_or(0);
            claims.push(Claim::new(
                format!("{who}: \"{shown}\" sits within {tolerance_dp} dp"),
                off <= tol,
                format!("at ({}, {}) {}×{} dp; the original at ({}, {}) {}×{} dp", b[0], b[1], b[2], b[3], a[0], a[1], a[2], a[3]),
            ));
        }
    }
    claims
}

/// Claims that hold, as a percentage (0 for none at all).
pub fn likeness(claims: &[Claim]) -> f64 {
    if claims.is_empty() {
        return 0.0;
    }
    claims.iter().filter(|c| c.passed).count() as f64 * 100.0 / claims.len() as f64
}

/// One screen's result in the scorecard.
#[derive(Debug, Clone, Serialize)]
pub struct StateScore {
    pub id: String,
    pub name: String,
    pub likeness: f64,
    pub claims: Vec<Claim>,
    /// Where the build's dump and picture of it are, relative to the project folder.
    pub files: Option<String>,
}

/// The package an APK declares in its manifest: what the build is installed and driven as. A
/// rebuild has its own id (and `.dev` for a debug build), never the original's, so the check
/// reads it from the APK rather than taking the target's.
pub fn apk_package(apk: &Path) -> Option<String> {
    let bytes = std::fs::read(apk).ok()?;
    let archive = craft_anything_formats::zip::parse(&bytes, 100_000).ok()?;
    let entry = archive.entries.iter().find(|e| e.name == "AndroidManifest.xml")?;
    let manifest = craft_anything_formats::zip::read_entry(&bytes, entry, 4 << 20).ok()?;
    let doc = craft_anything_formats::axml::parse(&manifest).ok()?;
    craft_anything_formats::axml::manifest(&doc).package.filter(|p| !p.trim().is_empty())
}

/// `device.json`: the scorecard of one device pass. `package` is the build's, `original` the
/// app it is judged against.
pub fn scorecard(serial: &str, package: &str, original: &str, apk: &Path, states: &[StateScore], crashed: Option<bool>, gate: Option<Value>) -> Value {
    let all: Vec<&Claim> = states.iter().flat_map(|s| &s.claims).collect();
    let held = all.iter().filter(|c| c.passed).count();
    json!({
        "version": 1,
        "serial": serial,
        "package": package,
        "original_package": original,
        "apk": apk,
        "screens": states.len(),
        "claims": all.len(),
        "held": held,
        "likeness": if all.is_empty() { 0.0 } else { held as f64 * 100.0 / all.len() as f64 },
        "crashed": crashed,
        "states": states,
        "gate": gate,
    })
}

/// The screens with the most failed claims, as "Name NN%" (at most `n`), weakest first.
pub fn weakest(states: &[StateScore], n: usize) -> Vec<String> {
    let mut v: Vec<&StateScore> = states.iter().filter(|s| s.claims.iter().any(|c| !c.passed)).collect();
    v.sort_by(|a, b| a.likeness.partial_cmp(&b.likeness).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
    v.into_iter().take(n).map(|s| format!("{} {} {:.0}%", s.id, s.name, s.likeness)).collect()
}

/// The screens a device check found weakest, from its flat claims (titles start with the
/// screen's id and name): for the refine message, which has only the `CheckResult`.
pub fn weakest_from_claims(claims: &[Claim], n: usize) -> Vec<String> {
    let mut by_screen: Vec<(String, usize, usize)> = Vec::new();
    for c in claims {
        let Some((who, _)) = c.title.split_once(':') else { continue };
        match by_screen.iter_mut().find(|(w, ..)| w == who) {
            Some((_, total, failed)) => {
                *total += 1;
                *failed += usize::from(!c.passed);
            }
            None => by_screen.push((who.to_string(), 1, usize::from(!c.passed))),
        }
    }
    by_screen.retain(|(_, _, failed)| *failed > 0);
    by_screen.sort_by(|a, b| (b.2 * a.1).cmp(&(a.2 * b.1)).then_with(|| a.0.cmp(&b.0)));
    by_screen.into_iter().take(n).map(|(who, total, failed)| format!("{who} ({} of {total} claims fail)", failed)).collect()
}

/// The check: install, replay, judge; then the gate device when set and the first was good enough.
pub struct DeviceCheck;

impl DeviceCheck {
    /// The device a build is checked on: the setting, else the target's own device.
    fn serial_of(cx: &CheckCx) -> Option<String> {
        cx.build_settings().android_serial.filter(|s| !s.trim().is_empty()).or_else(|| match &cx.ws.manifest.target {
            TargetRef::Device { serial, .. } => Some(serial.clone()),
            _ => None,
        })
    }

    /// One pass on one device: install, every kept screen, the crash log. `dir` is this pass's
    /// folder; returns the states, whether it crashed, and the installed APK.
    fn pass(cx: &mut CheckCx, serial: &str, package: &str, apk: &Path, screens: &Value, dir: &Path) -> Result<(Vec<StateScore>, bool), ToolError> {
        let build = cx.build_settings();
        cx.call("adb_app_install", json!({"serial": serial, "apk_path": apk, "replace": true}))?;
        let category = screens.get("launch_category").and_then(Value::as_str).unwrap_or("leanback");
        let timing = &screens["timing"];
        let wait_ms = timing.get("wait_ms").and_then(Value::as_u64).unwrap_or(u64::from(build.android_wait_ms));
        let lead_ms = timing.get("dump_lead_ms").and_then(Value::as_u64).unwrap_or(u64::from(build.android_dump_lead_ms));
        let launch_wait = Duration::from_millis(timing.get("launch_wait_ms").and_then(Value::as_u64).unwrap_or(u64::from(build.android_launch_wait_ms)));
        let map_dir = cx.ws.capture_dir().join("map");
        let read = |p: &Path| std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        // Each screen's capture makes its own folder and needs the parent to be there.
        let states_dir = dir.join("states");
        std::fs::create_dir_all(&states_dir).map_err(|e| ToolError::io(states_dir.display(), e))?;
        let mut states = Vec::new();
        let kept = screens.get("states").and_then(Value::as_array).into_iter().flatten().filter(|s| s.get("skipped").is_none_or(Value::is_null)).take(60);
        for state in kept {
            cx.stop().check()?;
            let id = state.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
            let name = state.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let path: Vec<String> =
                state.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            let original = read(&map_dir.join("states").join(&id).join("ui.json")).unwrap_or(Value::Null);
            let out = dir.join("states").join(&id);
            let _ = cx.call("adb_app_force_stop", json!({"serial": serial, "package": package}));
            let launched = cx.call("adb_app_launch", json!({"serial": serial, "package": package, "category": category})).is_ok();
            let mut built = Value::Null;
            if launched {
                std::thread::sleep(launch_wait);
                let got = cx.call(
                    "adb_screen_state_capture",
                    json!({"serial": serial, "keys": path, "wait_ms": wait_ms, "dump_lead_ms": lead_ms, "output_directory": out.display().to_string()}),
                );
                if got.is_ok() {
                    built = read(&out.join("ui.json")).unwrap_or(Value::Null);
                }
            }
            let claims = state_claims(&id, &name, &original, &built, build.android_dp_tolerance);
            let files = out.strip_prefix(&cx.ws.root).ok().map(|p| p.to_string_lossy().to_string()).filter(|_| out.join("ui.json").is_file());
            states.push(StateScore { likeness: likeness(&claims), id, name, claims, files });
        }
        let crashed = match cx.call("adb_crash_log_read", json!({"serial": serial, "package": package, "count": 500})) {
            Ok(ToolOutput::Evidence(e)) => e.result.get("crashed").and_then(Value::as_bool).unwrap_or(false),
            _ => false,
        };
        Ok((states, crashed))
    }
}

impl Check for DeviceCheck {
    fn id(&self) -> &'static str {
        DEVICE
    }

    /// With a map to replay and a device to replay it on.
    fn applies(&self, cx: &CheckCx) -> bool {
        cx.ws.capture_dir().join("map").join("screens.json").is_file() && Self::serial_of(cx).is_some()
    }

    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let round = cx.round;
        let Some(serial) = Self::serial_of(cx) else {
            return Ok(CheckResult::new(DEVICE, round, false, "No device to check on: set build.android_serial or use a device target."));
        };
        let original = match &cx.ws.manifest.target {
            TargetRef::Device { package, .. } => package.clone(),
            _ => {
                return Ok(CheckResult::new(
                    DEVICE,
                    round,
                    false,
                    "The device check needs a device target (adb:<serial>/<package>) to know the app's package.",
                ));
            }
        };
        let Some(apk) = super::super::check::debug_apk(&cx.ws.app_dir()) else {
            return Ok(CheckResult::new(DEVICE, round, false, format!("No debug APK under {}: build with ./gradlew assembleDebug first.", cx.ws.app_place())));
        };
        // The build is installed and driven as the package its own manifest declares; the
        // original's package is only what it is judged against, and is never launched here.
        let Some(package) = apk_package(&apk) else {
            return Ok(CheckResult::new(
                DEVICE,
                round,
                false,
                format!("Couldn't read the package name from {}: is it a complete APK with an AndroidManifest.xml?", apk.display()),
            ));
        };
        if package == original {
            return Ok(CheckResult::new(
                DEVICE,
                round,
                false,
                format!(
                    "The build declares the original's own package ({original}): give it its own applicationId, so the check never replaces or drives the original."
                ),
            ));
        }
        let screens = std::fs::read(cx.ws.capture_dir().join("map").join("screens.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        let dir = cx.ws.checks_dir().join(format!("round-{round}")).join(DEVICE);
        std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
        let good_enough = cx.build_settings().good_enough;
        let gate_serial = cx.build_settings().android_gate_serial.filter(|s| !s.trim().is_empty() && *s != serial);
        let (states, crashed) = match Self::pass(cx, &serial, &package, &apk, &screens, &dir) {
            Ok(v) => v,
            Err(e) => return Ok(CheckResult::new(DEVICE, round, false, format!("Couldn't install or drive the build on {serial}: {}", e.message))),
        };
        let mut claims: Vec<Claim> = states.iter().flat_map(|s| s.claims.clone()).collect();
        claims.push(Claim::new(format!("No crash of {package} on {serial}"), !crashed, "the device's crash log names it"));
        let read_any = states.iter().any(|s| s.files.is_some());
        let mut unknowns: Vec<String> =
            states.iter().filter(|s| s.files.is_none()).map(|s| format!("{} {} couldn't be read on the device", s.id, s.name)).collect();
        let mut score = likeness(&claims);
        let mut note = format!(
            "Device check on {serial}: {score:.0}% alike over {} screen(s) ({} of {} claims hold).",
            states.len(),
            claims.iter().filter(|c| c.passed).count(),
            claims.len()
        );
        let weak = weakest(&states, 3);
        if !weak.is_empty() {
            note.push_str(&format!(" Weakest: {}.", weak.join(", ")));
        }
        if crashed {
            note.push_str(&format!(" {package} crashed on the device: see the crash claim."));
        }
        // The gate: the same pass on the second device, once the first is good enough.
        let mut gate = None;
        let mut gate_ok = true;
        if let Some(gs) = gate_serial {
            if score >= good_enough && !crashed {
                let gdir = dir.join("gate");
                std::fs::create_dir_all(&gdir).map_err(|e| ToolError::io(gdir.display(), e))?;
                match Self::pass(cx, &gs, &package, &apk, &screens, &gdir) {
                    Ok((gstates, gcrashed)) => {
                        let mut gclaims: Vec<Claim> =
                            gstates.iter().flat_map(|s| s.claims.clone()).map(|c| Claim { title: format!("Test device: {}", c.title), ..c }).collect();
                        gclaims.push(Claim::new(format!("Test device: no crash of {package} on {gs}"), !gcrashed, "the device's crash log names it"));
                        let gscore = likeness(&gclaims);
                        gate_ok = !gcrashed && gscore >= good_enough;
                        note.push_str(&format!(" On the test device {gs}: {gscore:.0}% alike{}.", if gcrashed { ", and it crashed" } else { "" }));
                        gate = Some(scorecard(&gs, &package, &original, &apk, &gstates, Some(gcrashed), None));
                        score = score.min(gscore);
                        claims.extend(gclaims);
                    }
                    Err(e) => {
                        gate_ok = false;
                        unknowns.push(format!("the test device {gs} couldn't be checked: {}", e.message));
                        note.push_str(&format!(" The test device {gs} couldn't be checked: {}.", e.message));
                    }
                }
            } else {
                note.push_str(&format!(" The test device {gs} waits until the build device is good enough."));
            }
        }
        let card = scorecard(&serial, &package, &original, &apk, &states, Some(crashed), gate);
        let file = dir.join("device.json");
        let text = serde_json::to_string_pretty(&card).map_err(|e| ToolError::internal(e.to_string()))?;
        craft_anything_engine::project::write_atomic(&file, text.as_bytes())?;
        let artifacts = file.strip_prefix(&cx.ws.root).ok().map(Path::to_path_buf).into_iter().collect();
        let passed = read_any && !crashed && gate_ok;
        Ok(CheckResult { similarity: Some(score), claims, unknowns, artifacts, ..CheckResult::new(DEVICE, round, passed, note) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(focus: &str, controls: &[(&str, [i64; 4])]) -> Value {
        let nodes: Vec<Value> = controls
            .iter()
            .map(|(t, dp)| json!({"title": t, "role": "text", "focusable": true, "focused": *t == focus, "dp": {"x": dp[0], "y": dp[1], "width": dp[2], "height": dp[3]}}))
            .collect();
        json!({"count": nodes.len(), "focused_title": focus, "nodes": nodes})
    }

    #[test]
    fn claims_say_what_matches_and_what_is_off() {
        let original = tree("Guide", &[("Guide", [72, 251, 220, 39]), ("Settings", [72, 300, 220, 39]), ("Search", [72, 350, 220, 39])]);
        let same = state_claims("s01", "Home", &original, &original, 1);
        assert!(same.iter().all(|c| c.passed), "{same:?}");
        assert_eq!(same.len(), 1 + 1 + 3 + 3, "read, focus, 3 controls, 3 places");
        assert_eq!(likeness(&same), 100.0);

        // The focus moved, one control is missing, one sits 2 dp off.
        let built = tree("Settings", &[("guide ", [72, 253, 220, 39]), ("Settings", [72, 300, 220, 39])]);
        let claims = state_claims("s01", "Home", &original, &built, 1);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["s01 Home: the focus is on \"Guide\"", "s01 Home: \"Guide\" sits within 1 dp", "s01 Home: it has \"Search\""]);
        assert!(claims.iter().any(|c| c.title.ends_with("it has \"Guide\"") && c.passed), "labels match without case or trailing spaces");
        assert!(claims.iter().find(|c| c.title.contains("sits within")).unwrap().detail.contains("(72, 253)"));
        assert!((likeness(&claims) - 400.0 / 7.0).abs() < 0.01, "4 of 7 claims hold: {}", likeness(&claims));
        assert!(state_claims("s01", "Home", &original, &built, 2).iter().filter(|c| !c.passed).count() == 2, "2 dp is within a tolerance of 2");

        // An unreadable screen is one failed claim; nothing else can be said.
        let empty = state_claims("s02", "Player", &original, &json!({"count": 0, "nodes": []}), 1);
        assert_eq!(empty.len(), 1);
        assert!(!empty[0].passed && empty[0].title.contains("read on the device"));
        assert_eq!(likeness(&[]), 0.0);
    }

    #[test]
    fn a_wordless_control_is_known_by_the_words_inside_it() {
        // Two Compose buttons: each a focusable group with no text and a text child.
        let compose = |focus: &str, a: (&str, [i64; 4]), b: (&str, [i64; 4])| {
            let group = |t: &str, dp: [i64; 4]| json!({"depth": 6, "title": "", "role": "group", "focusable": true, "focused": t == focus, "dp": {"x": dp[0], "y": dp[1], "width": dp[2], "height": dp[3]}});
            let text = |t: &str| json!({"depth": 7, "title": t, "role": "text", "focusable": false});
            let nodes = vec![
                json!({"depth": 5, "title": "", "role": "group"}),
                json!({"depth": 6, "title": "Trial home", "role": "text"}),
                group(a.0, a.1),
                text(a.0),
                group(b.0, b.1),
                text(b.0),
            ];
            json!({"count": nodes.len(), "focused_title": focus, "nodes": nodes})
        };
        let original = compose("Channels", ("Channels", [48, 112, 108, 48]), ("Settings", [48, 172, 101, 48]));
        assert_eq!(landable(&original).iter().map(|(_, s, _)| s.as_str()).collect::<Vec<_>>(), ["Channels", "Settings"]);
        let same = state_claims("s01", "Trial home", &original, &original, 1);
        assert_eq!(same.len(), 1 + 1 + 2 + 2, "read, focus, 2 controls, 2 places: {same:?}");
        assert!(same.iter().all(|c| c.passed));
        // The second button moved down 3 dp and says something else.
        let built = compose("Channels", ("Channels", [48, 112, 108, 48]), ("Options", [48, 175, 101, 48]));
        let claims = state_claims("s01", "Trial home", &original, &built, 1);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["s01 Trial home: it has \"Settings\""]);
        let moved = compose("Channels", ("Channels", [48, 112, 108, 48]), ("Settings", [48, 175, 101, 48]));
        let claims = state_claims("s01", "Trial home", &original, &moved, 1);
        let failed: Vec<&str> = claims.iter().filter(|c| !c.passed).map(|c| c.title.as_str()).collect();
        assert_eq!(failed, ["s01 Trial home: \"Settings\" sits within 1 dp"]);
    }

    #[test]
    fn the_scorecard_and_the_weakest_screens() {
        let a = StateScore {
            id: "s01".into(),
            name: "Home".into(),
            likeness: 100.0,
            claims: vec![Claim::new("s01 Home: x", true, "")],
            files: Some("checks/round-1/device/states/s01".into()),
        };
        let b = StateScore {
            id: "s02".into(),
            name: "Guide".into(),
            likeness: 25.0,
            claims: vec![
                Claim::new("s02 Guide: a", true, ""),
                Claim::new("s02 Guide: b", false, "no"),
                Claim::new("s02 Guide: c", false, "no"),
                Claim::new("s02 Guide: d", false, "no"),
            ],
            files: None,
        };
        let c = StateScore {
            id: "s03".into(),
            name: "Settings".into(),
            likeness: 50.0,
            claims: vec![Claim::new("s03 Settings: a", true, ""), Claim::new("s03 Settings: b", false, "no")],
            files: None,
        };
        let card = scorecard(
            "192.168.1.20:5555",
            "com.example.tv.dev",
            "com.example.tv",
            Path::new("/p/app/tv/build/outputs/apk/debug/tv-debug.apk"),
            &[a.clone(), b.clone(), c.clone()],
            Some(false),
            None,
        );
        assert_eq!((card["screens"].as_u64(), card["claims"].as_u64(), card["held"].as_u64()), (Some(3), Some(7), Some(3)));
        assert!((card["likeness"].as_f64().unwrap() - 300.0 / 7.0).abs() < 0.01);
        assert_eq!(card["states"][1]["name"], "Guide");
        assert_eq!(weakest(&[a.clone(), b.clone(), c.clone()], 3), ["s02 Guide 25%", "s03 Settings 50%"]);
        assert_eq!(weakest(&[a], 3), Vec::<String>::new());
        let flat: Vec<Claim> = [b, c].iter().flat_map(|s| s.claims.clone()).chain([Claim::new("No crash of com.example.tv on x", true, "")]).collect();
        assert_eq!(weakest_from_claims(&flat, 2), ["s02 Guide (3 of 4 claims fail)", "s03 Settings (1 of 2 claims fail)"]);
        assert_eq!((card["package"].as_str(), card["original_package"].as_str()), (Some("com.example.tv.dev"), Some("com.example.tv")));
    }

    #[test]
    fn the_build_is_driven_as_the_package_its_apk_declares() {
        // The APK's own manifest names what gets installed and launched, not the target's package.
        let d = tempfile::tempdir().unwrap();
        let apk = d.path().join("tv-debug.apk");
        std::fs::write(&apk, craft_anything_testkit::android::notes_apk()).unwrap();
        assert_eq!(apk_package(&apk).as_deref(), Some(craft_anything_testkit::android::PACKAGE));
        let not_apk = d.path().join("notes.txt");
        std::fs::write(&not_apk, b"not a zip").unwrap();
        assert_eq!(apk_package(&not_apk), None);
        assert_eq!(apk_package(&d.path().join("missing.apk")), None);
    }
}
