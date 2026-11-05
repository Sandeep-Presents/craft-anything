//! The device check for an Android app: the debug APK is installed on the build device, each
//! screen of the map is reached again by its keys, and claims judge it against the original's
//! screen (the focus, the controls by their text, their places in dp, no crash). Once the build
//! device is good enough and a second device is set, the same runs there as a gate.
//!
//! The judging is pure ([`state_claims`], [`likeness`], [`scorecard`], [`weakest`]); only
//! [`DeviceCheck::run`] touches a device.

use super::*;
use craft_anything_engine::tools::web_common::{Image, NoiseMask, block_similarity_masked, decode_png, perceptual_similarity_masked, resize};
use craft_anything_engine::workspace::Claim;
use serde::Serialize;

/// The check's id, and its folder under checks/round-N/.
pub const DEVICE: &str = "device";

/// Labels as compared: without case, extra spacing or a trailing colon.
/// What makes two labels the same control: spacing, case and a trailing colon don't, and no
/// run of digits does (a guide cell says "Programme 22609" at map time and "Programme 27796" an
/// hour later; a clock moves on), as the screen fingerprint reads them.
fn key_of(label: &str) -> String {
    craft_anything_engines::adb_ui::digits_as_one(&label.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(':').trim().to_lowercase())
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

/// The check's own output folder, emptied: a builder may check its work more than once in a
/// round, and `adb_screen_state_capture` refuses a state folder that is already there, so a
/// second pass over the last one's states would read every screen as unreadable.
fn fresh_dir(dir: &Path) -> Result<(), ToolError> {
    if dir.is_dir() {
        std::fs::remove_dir_all(dir).map_err(|e| ToolError::io(dir.display(), e))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| ToolError::io(dir.display(), e))
}

/// The claims that must hold for a pass whatever its likeness: each screen reached and read
/// (the first claim `state_claims` makes for a screen). The run judges the rest by likeness.
pub fn must_hold(states: &[StateScore]) -> Vec<Claim> {
    states.iter().filter_map(|s| s.claims.first().cloned()).collect()
}

/// Whether the build's dump is worth reading again for its focus: the original's screen has a
/// focused control, the build's was read with controls, but nothing on it has the focus yet.
pub fn focus_not_yet(original: &Value, built: &Value) -> bool {
    let has_focus = |t: &Value| t.get("focused_title").and_then(Value::as_str).is_some_and(|f| !f.trim().is_empty());
    let read = built.get("count").and_then(Value::as_u64).unwrap_or(0) > 0;
    has_focus(original) && read && !has_focus(built)
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
    /// How alike the build's picture of the screen is to the original's, in percent (None when
    /// the screen wasn't read, or either picture couldn't be).
    pub picture: Option<f64>,
}

/// The size both pictures are brought to before they are compared (the screenshot check's).
const COMPARE: (u32, u32) = (640, 360);

/// A screen's two pictures, the original's and the build's, at the compare size. None when
/// either can't be read, and when the original's is blank (a video surface the screenshot
/// can't see, an overlay that had already gone when the map took it): nothing can be said about
/// such a screen, and a black build picture would otherwise score a free 100% there.
fn picture_pair(original: &Path, built: &Path) -> Option<(Image, Image)> {
    let read = |p: &Path| std::fs::read(p).ok().and_then(|b| decode_png(&b).ok());
    let theirs = read(original)?;
    if super::screenshot::is_blank(&theirs) {
        return None;
    }
    let ours = read(built)?;
    Some((resize(&theirs, COMPARE.0, COMPARE.1), resize(&ours, COMPARE.0, COMPARE.1)))
}

/// The screenshot check's measure, in percent: perceptual detail and coarse layout, over the
/// tiles `mask` leaves in. Colours, type, spacing and icon glyphs count, which a dump never shows.
pub fn pair_score(original: &Image, built: &Image, mask: Option<&NoiseMask>) -> f64 {
    (perceptual_similarity_masked(original, built, mask) + block_similarity_masked(original, built, 16, 24, mask)) * 50.0
}

/// How alike a screen's picture is to the original's, in percent ([`pair_score`] of
/// [`picture_pair`]); None when there is nothing to compare.
pub fn picture_score(original: &Path, built: &Path) -> Option<f64> {
    picture_pair(original, built).map(|(theirs, ours)| pair_score(&theirs, &ours, None))
}

/// What two devices photograph differently of the same build of a screen (a video plane one of
/// them composes into its screenshot and the other doesn't, a clock) is the device's doing, not
/// the build's: once the test device has been pictured too, the tiles that differ between the
/// two build pictures are left out of both scores against the original, and each screen's
/// picture claim is made again from its new score. `root` is where each state's `files` lead.
pub fn rescore_without_device_noise(states: &mut [StateScore], gstates: &mut [StateScore], root: &Path, map_dir: &Path, bar: Option<f64>) {
    for s in states.iter_mut() {
        let Some(g) = gstates.iter_mut().find(|g| g.id == s.id) else { continue };
        let (Some(sf), Some(gf)) = (s.files.clone(), g.files.clone()) else { continue };
        let original = map_dir.join("states").join(&s.id).join("screen.png");
        let (Some((theirs, ours)), Some((_, gours))) =
            (picture_pair(&original, &root.join(sf).join("screen.png")), picture_pair(&original, &root.join(gf).join("screen.png")))
        else {
            continue;
        };
        let noise = NoiseMask::between(&ours, &gours, 24);
        s.picture = Some(pair_score(&theirs, &ours, Some(&noise)));
        g.picture = Some(pair_score(&theirs, &gours, Some(&noise)));
        if let Some(bar) = bar {
            replace_picture_claim(s, bar);
            replace_picture_claim(g, bar);
        }
    }
}

/// A screen's picture claim made again from its current score (in place of the one it had).
fn replace_picture_claim(st: &mut StateScore, bar: f64) {
    let claim = picture_claim(&format!("{} {}", st.id, st.name), st.picture, bar);
    match st.claims.iter_mut().find(|c| c.title == claim.title) {
        Some(c) => *c = claim,
        None => st.claims.push(claim),
    }
    st.likeness = likeness(&st.claims);
}

/// The claim a picture bar (`build.picture_good_enough`) adds to a screen that was read: its
/// picture is at least that alike. A screen read but not pictured fails it, saying so.
pub fn picture_claim(who: &str, picture: Option<f64>, bar: f64) -> Claim {
    let title = format!("{who}: it looks at least {bar:.0}% like the original");
    match picture {
        Some(p) => Claim::new(title, p >= bar, format!("{p:.0}% alike")),
        None => Claim::new(title, false, "the build's picture couldn't be read"),
    }
}

/// The mean picture likeness of the screens that were pictured; None when none was.
pub fn picture_mean(states: &[StateScore]) -> Option<f64> {
    let v: Vec<f64> = states.iter().filter_map(|s| s.picture).collect();
    if v.is_empty() { None } else { Some(v.iter().fold(0.0, |a, b| a + b) / v.len() as f64) }
}

/// The screens whose pictures look least like the original's, as "id Name NN%" (at most `n`).
pub fn least_alike(states: &[StateScore], n: usize) -> Vec<String> {
    let mut v: Vec<(&StateScore, f64)> = states.iter().filter_map(|s| s.picture.map(|p| (s, p))).collect();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.id.cmp(&b.0.id)));
    v.into_iter().take(n).map(|(s, p)| format!("{} {} {p:.0}%", s.id, s.name)).collect()
}

/// The likeness the run judges a pass by: the claims that hold, and with a picture bar set no
/// more than the pictures' mean, so a build can't be Ready on its structure alone.
pub fn device_score(claims: f64, pictures: Option<f64>, bar: Option<f64>) -> f64 {
    match (bar, pictures) {
        (Some(_), Some(p)) => claims.min(p),
        _ => claims,
    }
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
        "picture_likeness": picture_mean(states),
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
        // A TV that fell asleep since it was last driven (the test device waits an hour between
        // passes) launches the app with its screen off and returns empty dumps: wake it first.
        let _ = cx.call("adb_keys_press", json!({"serial": serial, "keys": ["WAKEUP"]}));
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
                // The focus lands a moment after the controls are drawn (Compose asks for it
                // once the screen is up): a dump with nothing focused is read once more.
                if focus_not_yet(&original, &built) {
                    std::thread::sleep(Duration::from_millis(wait_ms.min(10_000)));
                    let again = match cx.call("adb_ui_read", json!({"serial": serial})) {
                        Ok(ToolOutput::Evidence(e)) => e.result.clone(),
                        _ => Value::Null,
                    };
                    if again.get("focused_title").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()) {
                        if let Ok(text) = serde_json::to_vec_pretty(&again) {
                            let _ = craft_anything_engine::project::write_atomic(&out.join("ui.json"), &text);
                        }
                        built = again;
                    }
                }
            }
            let mut claims = state_claims(&id, &name, &original, &built, build.android_dp_tolerance);
            // The look, which the dump can't show: the two pictures side by side (a blank
            // picture of the original, a video surface or an overlay already gone, is skipped).
            let read = built.get("count").and_then(Value::as_u64).unwrap_or(0) > 0;
            let picture = if read { picture_score(&map_dir.join("states").join(&id).join("screen.png"), &out.join("screen.png")) } else { None };
            if let (Some(bar), true) = (build.picture_good_enough, read) {
                claims.push(picture_claim(&format!("{id} {name}"), picture, bar));
            }
            let files = out.strip_prefix(&cx.ws.root).ok().map(|p| p.to_string_lossy().to_string()).filter(|_| out.join("ui.json").is_file());
            states.push(StateScore { likeness: likeness(&claims), id, name, claims, files, picture });
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
        fresh_dir(&dir)?;
        let good_enough = cx.build_settings().good_enough;
        let picture_bar = cx.build_settings().picture_good_enough;
        let gate_serial = cx.build_settings().android_gate_serial.filter(|s| !s.trim().is_empty() && *s != serial);
        let (states, crashed) = match Self::pass(cx, &serial, &package, &apk, &screens, &dir) {
            Ok(v) => v,
            Err(e) => return Ok(CheckResult::new(DEVICE, round, false, format!("Couldn't install or drive the build on {serial}: {}", e.message))),
        };
        let crash_claim = |crashed: bool| Claim::new(format!("No crash of {package} on {serial}"), !crashed, "the device's crash log names it");
        let flat = |states: &[StateScore]| -> Vec<Claim> { states.iter().flat_map(|s| s.claims.clone()).collect() };
        // The gate: the same pass on the second device, once the first is good enough by its
        // own claims and pictures. What the two devices then photograph differently of one
        // build (a video plane one of them composes into its screenshot, a clock) is the
        // device's doing, not the build's, and is left out of both pictures' scores.
        let mut states = states;
        let mut gate_pass = None;
        let mut gate_error = None;
        let mut gate_waits = None;
        let first = {
            let mut c = flat(&states);
            c.push(crash_claim(crashed));
            device_score(likeness(&c), picture_mean(&states), picture_bar)
        };
        if let Some(gs) = gate_serial {
            if first >= good_enough && !crashed {
                let gdir = dir.join("gate");
                fresh_dir(&gdir)?;
                match Self::pass(cx, &gs, &package, &apk, &screens, &gdir) {
                    Ok((mut gstates, gcrashed)) => {
                        let map_dir = cx.ws.capture_dir().join("map");
                        rescore_without_device_noise(&mut states, &mut gstates, &cx.ws.root, &map_dir, picture_bar);
                        gate_pass = Some((gs, gstates, gcrashed));
                    }
                    Err(e) => gate_error = Some((gs, e.message)),
                }
            } else {
                gate_waits = Some(gs);
            }
        }
        let mut claims = flat(&states);
        claims.push(crash_claim(crashed));
        // What the run is handed: the claims that must hold (every screen reached and read, no
        // crash), while the likeness is the bar for the rest. Every detail claim stays in the
        // scorecard and names the weakest screens in the note; handing them all over would ask
        // for 100% before Ready, whatever good_enough says.
        let mut musts = must_hold(&states);
        musts.push(crash_claim(crashed));
        let read_any = states.iter().any(|s| s.files.is_some());
        let mut unknowns: Vec<String> =
            states.iter().filter(|s| s.files.is_none()).map(|s| format!("{} {} couldn't be read on the device", s.id, s.name)).collect();
        let pictures = picture_mean(&states);
        let mut score = device_score(likeness(&claims), pictures, picture_bar);
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
        if let Some(p) = pictures {
            let bar = picture_bar.map(|b| format!(", the bar is {b:.0}%")).unwrap_or_default();
            note.push_str(&format!(" Pictures: {p:.0}% like the original's on average{bar}."));
            let least = least_alike(&states, 3);
            if !least.is_empty() {
                note.push_str(&format!(" Least alike: {}.", least.join(", ")));
            }
        }
        if crashed {
            note.push_str(&format!(" {package} crashed on the device: see the crash claim."));
        }
        let mut gate = None;
        let mut gate_ok = true;
        if let Some((gs, gstates, gcrashed)) = gate_pass {
            let mut gclaims: Vec<Claim> = flat(&gstates).into_iter().map(|c| Claim { title: format!("Test device: {}", c.title), ..c }).collect();
            gclaims.push(Claim::new(format!("Test device: no crash of {package} on {gs}"), !gcrashed, "the device's crash log names it"));
            musts.extend(must_hold(&gstates).into_iter().map(|c| Claim { title: format!("Test device: {}", c.title), ..c }));
            musts.push(Claim::new(format!("Test device: no crash of {package} on {gs}"), !gcrashed, "the device's crash log names it"));
            let gpictures = picture_mean(&gstates);
            let gscore = device_score(likeness(&gclaims), gpictures, picture_bar);
            gate_ok = !gcrashed && gscore >= good_enough;
            note.push_str(&format!(" On the test device {gs}: {gscore:.0}% alike{}.", if gcrashed { ", and it crashed" } else { "" }));
            if let Some(p) = gpictures {
                note.push_str(&format!(" Its pictures: {p:.0}% on average, what the two devices photograph differently left out."));
                let least = least_alike(&gstates, 3);
                if !least.is_empty() {
                    note.push_str(&format!(" Least alike there: {}.", least.join(", ")));
                }
            }
            gate = Some(scorecard(&gs, &package, &original, &apk, &gstates, Some(gcrashed), None));
            score = score.min(gscore);
            claims.extend(gclaims);
        }
        if let Some((gs, message)) = gate_error {
            gate_ok = false;
            unknowns.push(format!("the test device {gs} couldn't be checked: {message}"));
            note.push_str(&format!(" The test device {gs} couldn't be checked: {message}."));
        }
        if let Some(gs) = gate_waits {
            note.push_str(&format!(" The test device {gs} waits until the build device is good enough."));
        }
        let card = scorecard(&serial, &package, &original, &apk, &states, Some(crashed), gate);
        let file = dir.join("device.json");
        let text = serde_json::to_string_pretty(&card).map_err(|e| ToolError::internal(e.to_string()))?;
        craft_anything_engine::project::write_atomic(&file, text.as_bytes())?;
        let artifacts = file.strip_prefix(&cx.ws.root).ok().map(Path::to_path_buf).into_iter().collect();
        let passed = read_any && !crashed && gate_ok;
        Ok(CheckResult { similarity: Some(score), claims: musts, unknowns, artifacts, ..CheckResult::new(DEVICE, round, passed, note) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_engine::tools::web_common::encode_png;

    fn picture(w: u32, h: u32, f: &dyn Fn(u32, u32) -> [u8; 3]) -> Image {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
                rgba.push(255);
            }
        }
        Image { width: w, height: h, rgba }
    }

    /// A dark screen with a light bar at the foot: one device photographs it as drawn, the
    /// other composes the video plane into its screenshot. Against the original, the second
    /// scores badly on its own and as well as the first once the device noise is left out.
    #[test]
    fn what_two_devices_photograph_differently_is_the_device_s_doing() {
        let ui = |_: u32, y: u32| if y >= 300 { [220, 220, 220] } else { [16, 16, 16] };
        let original = picture(640, 360, &ui);
        let plain = picture(640, 360, &ui);
        let with_video = picture(640, 360, &|x, y| if y < 280 { [(x / 80 * 30) as u8, 200, 100] } else { ui(x, y) });
        assert!(pair_score(&original, &with_video, None) < 60.0, "{}", pair_score(&original, &with_video, None));
        let noise = NoiseMask::between(&plain, &with_video, 24);
        assert!(pair_score(&original, &with_video, Some(&noise)) > 99.0);
        assert!(pair_score(&original, &plain, Some(&noise)) > 99.0);
        // Through the files the check keeps: both screens rescored, the claims made again.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (p, img) in [("capture/map/states/s03/screen.png", &original), ("dev/s03/screen.png", &plain), ("gate/s03/screen.png", &with_video)] {
            let f = root.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, encode_png(img).unwrap()).unwrap();
        }
        let state = |files: &str, picture: f64| StateScore {
            id: "s03".into(),
            name: "Player".into(),
            likeness: 50.0,
            claims: vec![Claim::new("s03 Player: the screen is read on the device", true, ""), picture_claim("s03 Player", Some(picture), 80.0)],
            files: Some(files.into()),
            picture: Some(picture),
        };
        let mut states = vec![state("dev/s03", 99.0)];
        let mut gstates = vec![state("gate/s03", 40.0)];
        assert!(!gstates[0].claims[1].passed);
        rescore_without_device_noise(&mut states, &mut gstates, root, &root.join("capture/map"), Some(80.0));
        assert!(gstates[0].picture.unwrap() > 99.0 && states[0].picture.unwrap() > 99.0);
        assert!(gstates[0].claims.iter().all(|c| c.passed) && gstates[0].claims.len() == 2, "{:?}", gstates[0].claims);
        assert_eq!(gstates[0].likeness, 100.0);
        // Without a bar there is no claim to remake; a screen the gate didn't read is left alone.
        let mut unread = vec![StateScore { files: None, claims: vec![], picture: None, ..state("gate/s03", 40.0) }];
        rescore_without_device_noise(&mut states, &mut unread, root, &root.join("capture/map"), None);
        assert_eq!(unread[0].picture, None);
    }

    /// A picture bar claims each read screen's look and caps the pass's likeness by the
    /// pictures' mean; without one, the pictures are only reported.
    #[test]
    fn a_picture_bar_claims_the_look_and_caps_the_score() {
        let s = |id: &str, picture: Option<f64>| StateScore { id: id.into(), name: "Guide".into(), likeness: 95.0, claims: vec![], files: None, picture };
        let states = [s("s01", Some(100.0)), s("s02", Some(63.0)), s("s03", None)];
        assert_eq!(picture_mean(&states), Some(81.5));
        assert_eq!(least_alike(&states, 2), vec!["s02 Guide 63%", "s01 Guide 100%"]);
        assert_eq!(picture_mean(&[s("s04", None)]), None);
        let c = picture_claim("s02 Guide", Some(63.0), 80.0);
        assert!(!c.passed);
        assert_eq!((c.title.as_str(), c.detail.as_str()), ("s02 Guide: it looks at least 80% like the original", "63% alike"));
        assert!(picture_claim("s01 Guide", Some(80.0), 80.0).passed);
        let unread = picture_claim("s03 Guide", None, 80.0);
        assert!(!unread.passed && unread.detail.contains("couldn't be read"));
        assert_eq!(device_score(95.0, Some(81.5), Some(80.0)), 81.5);
        assert_eq!(device_score(95.0, Some(81.5), None), 95.0, "without a bar the pictures are only reported");
        assert_eq!(device_score(95.0, None, Some(80.0)), 95.0, "nothing pictured, nothing to cap by");
    }

    /// The picture score sees what the dump can't: the same picture is 100%, one with another
    /// colour over half of it is well under, and a missing or broken picture is None.
    #[test]
    fn a_screen_s_picture_is_scored_against_the_original_s() {
        let img = |f: &dyn Fn(u32, u32) -> [u8; 3]| picture(64, 36, f);
        let dir = tempfile::tempdir().unwrap();
        let scene = img(&|x, y| if (x / 8 + y / 6) % 2 == 0 { [20, 20, 24] } else { [200, 200, 210] });
        let tinted = img(&|x, y| {
            if x < 32 {
                [230, 120, 20]
            } else if (x / 8 + y / 6) % 2 == 0 {
                [20, 20, 24]
            } else {
                [200, 200, 210]
            }
        });
        let original = dir.path().join("original.png");
        let same = dir.path().join("same.png");
        let other = dir.path().join("other.png");
        std::fs::write(&original, encode_png(&scene).unwrap()).unwrap();
        std::fs::write(&same, encode_png(&scene).unwrap()).unwrap();
        std::fs::write(&other, encode_png(&tinted).unwrap()).unwrap();
        assert!(picture_score(&original, &same).is_some_and(|p| p > 99.9), "the same picture is 100%");
        let off = picture_score(&original, &other).unwrap();
        assert!(off < 80.0, "half the picture in another colour: {off}");
        assert_eq!(picture_score(&original, &dir.path().join("missing.png")), None);
        std::fs::write(&other, b"not a png").unwrap();
        assert_eq!(picture_score(&original, &other), None);
        assert_eq!(picture_score(&dir.path().join("missing.png"), &same), None, "no original to compare with");
        let black = img(&|_, _| [16, 16, 16]);
        let blank = dir.path().join("blank.png");
        std::fs::write(&blank, encode_png(&black).unwrap()).unwrap();
        assert_eq!(picture_score(&blank, &blank), None, "a blank original says nothing, even against a blank build");
        assert_eq!(picture_score(&blank, &same), None);
        assert!(picture_score(&original, &blank).is_some_and(|p| p < 80.0), "a blank build against a drawn original scores low");
    }

    /// Likeness is the bar: a pass at 92% with every screen read and no crash is good at 90,
    /// whatever its detail claims say; a screen that couldn't be read is not.
    #[test]
    fn the_run_is_handed_only_what_must_hold() {
        let screen = |id: &str, read: bool, detail_ok: bool| StateScore {
            id: id.into(),
            name: "Home".into(),
            likeness: if read { 50.0 } else { 0.0 },
            claims: vec![
                Claim::new(format!("{id} Home: the screen is read on the device"), read, ""),
                Claim::new(format!("{id} Home: it has \"Guide\""), detail_ok, "no control with that text"),
            ],
            files: read.then(|| format!("checks/round-0/device/states/{id}")),
            picture: None,
        };
        let states = vec![screen("s01", true, true), screen("s02", true, false)];
        let mut musts = must_hold(&states);
        assert_eq!(musts.len(), 2, "one claim per screen: that it was read");
        assert!(musts.iter().all(|c| c.passed && c.title.ends_with("read on the device")));
        musts.push(Claim::new("No crash", true, ""));
        let result = CheckResult { similarity: Some(92.0), claims: musts.clone(), ..CheckResult::new(DEVICE, 1, true, "") };
        assert!(result.good(90.0), "92% with every screen read is good at 90");
        assert!(!result.good(95.0), "and not at 95");
        let unread = must_hold(&[screen("s01", true, true), screen("s03", false, false)]);
        let result = CheckResult { similarity: Some(92.0), claims: unread, ..CheckResult::new(DEVICE, 1, true, "") };
        assert!(!result.good(90.0), "a screen that couldn't be read must hold first");
    }

    /// A builder that checks twice in one round: the second pass must not trip over the first's
    /// state folders.
    #[test]
    fn a_pass_starts_from_an_empty_device_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("round-0").join("device");
        std::fs::create_dir_all(dir.join("states").join("s01")).unwrap();
        std::fs::write(dir.join("states").join("s01").join("ui.json"), b"{}").unwrap();
        std::fs::write(dir.join("device.json"), b"{}").unwrap();
        fresh_dir(&dir).unwrap();
        assert!(dir.is_dir() && std::fs::read_dir(&dir).unwrap().next().is_none(), "the folder is there and empty");
        fresh_dir(&dir).unwrap();
        assert!(dir.is_dir(), "an empty folder is fine too");
    }

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
        // Data on the screen moves on between the map and the check: digits read as one.
        let then = tree(
            "Channel 23 News Programme 22609 & more",
            &[("Channel 23 News Programme 22609 & more", [72, 251, 220, 39]), ("07:30 — 08:00", [72, 300, 220, 39])],
        );
        let later = tree(
            "Channel 23 News Programme 27796 & more",
            &[("Channel 23 News Programme 27796 & more", [72, 251, 220, 39]), ("09:00 — 10:00", [72, 300, 220, 39])],
        );
        assert!(state_claims("s05", "Guide", &then, &later, 1).iter().all(|c| c.passed), "a later programme and a later time are the same controls");
        let other = tree("Channel 23 Sport", &[("Channel 23 Sport", [72, 251, 220, 39])]);
        assert!(!state_claims("s05", "Guide", &then, &other, 1).iter().all(|c| c.passed), "other words are another control");
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
    fn a_dump_with_nothing_focused_yet_is_read_again_only_when_the_original_has_a_focus() {
        let original = tree("Guide", &[("Guide", [72, 251, 220, 39])]);
        let mut unfocused = original.clone();
        unfocused["focused_title"] = Value::Null;
        assert!(focus_not_yet(&original, &unfocused));
        assert!(!focus_not_yet(&original, &original), "the focus is there");
        assert!(!focus_not_yet(&unfocused, &unfocused), "the original has none to wait for");
        assert!(!focus_not_yet(&original, &json!({"count": 0})), "an unread screen is a failed read, not a wait");
        assert!(!focus_not_yet(&original, &Value::Null));
    }

    #[test]
    fn the_scorecard_and_the_weakest_screens() {
        let a = StateScore {
            id: "s01".into(),
            name: "Home".into(),
            likeness: 100.0,
            claims: vec![Claim::new("s01 Home: x", true, "")],
            files: Some("checks/round-1/device/states/s01".into()),
            picture: None,
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
            picture: Some(63.0),
        };
        let c = StateScore {
            id: "s03".into(),
            name: "Settings".into(),
            likeness: 50.0,
            claims: vec![Claim::new("s03 Settings: a", true, ""), Claim::new("s03 Settings: b", false, "no")],
            files: None,
            picture: Some(97.0),
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
        assert!((card["picture_likeness"].as_f64().unwrap() - 80.0).abs() < 0.01, "the mean of the pictured screens: {}", card["picture_likeness"]);
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
