//! An app on a device: the notes Study writes from what the device says about itself and from
//! the map of the app's screens (`capture/map/screens.json`), for the builder to read first.

use serde_json::Value;

/// `capture/README.md` for a device target. `device` is `adb_device_show`'s result (or an
/// `error`), `screens` the map's screens.json when it was made, `map_note` why it wasn't.
pub fn readme(package: &str, serial: &str, device: &Value, screens: Option<&Value>, map_note: Option<&str>) -> String {
    let mut s = format!(
        "# {package} on {serial}\n\nAn app on a device is watched as a person uses it: nothing inside it (its files, code or packages) was read, in Clean room or Deep mode. Everything below came from the screen and the remote's keys.\n\n## The device\n\n"
    );
    let d = &device["device"];
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    if let Some(err) = device.get("error").and_then(Value::as_str) {
        s.push_str(&format!("The device didn't answer: {err}\n"));
    } else {
        let tv = device.get("tv").and_then(Value::as_bool).unwrap_or(false);
        s.push_str(&format!(
            "- {} {} ({}), Android {} (API {}): {}.\n",
            str_of(d, "ro.product.manufacturer"),
            str_of(d, "ro.product.model"),
            str_of(d, "ro.product.device"),
            str_of(d, "ro.build.version.release"),
            str_of(d, "ro.build.version.sdk"),
            if tv { "a TV, driven by a remote (D-pad, OK, BACK, MENU)" } else { "a phone or tablet" }
        ));
        let density = device.get("density_dpi").and_then(Value::as_u64);
        let (w, h) = (device["display_px"]["width"].as_u64(), device["display_px"]["height"].as_u64());
        match (density, w, h) {
            (Some(dpi), Some(w), Some(h)) => {
                s.push_str(&format!("- Display: {w}×{h} px at {dpi} dpi, that is {}×{} dp.\n", w * 160 / dpi.max(1), h * 160 / dpi.max(1)))
            }
            (Some(dpi), _, _) => s.push_str(&format!("- Density: {dpi} dpi.\n")),
            _ => s.push_str("- The display's density wasn't read: positions are in pixels only.\n"),
        }
        s.push_str("- Positions: ui.json gives each control's position and size in px and, under `dp`, in density-independent pixels (dp = px × 160 ÷ density). Build to the dp values; the device check pairs controls by their text and accepts ±1 dp.\n");
    }
    s.push_str("\n## Its screens\n\n");
    match screens {
        Some(m) => {
            let states = m.get("states").and_then(Value::as_array).cloned().unwrap_or_default();
            let kept = states.iter().filter(|st| st.get("skipped").is_none_or(Value::is_null)).count();
            s.push_str(&format!(
                "map/screens.json lists {} screen(s) reached from the app's home-screen entry with the keys {} ({} kept, {} left alone). Each kept screen has map/states/<id>/ui.json (its controls) and screen.png (how it looks); open the picture with the Read tool and match it. The keys in `path` reach the screen from a fresh launch; the device check replays exactly those on your build.\n\n",
                states.len(),
                m.get("keys").and_then(Value::as_array).map(|k| k.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                kept,
                states.len() - kept
            ));
            s.push_str("| Screen | Name | Keys from launch | Focus on | Files |\n|---|---|---|---|---|\n");
            for st in states.iter().take(200) {
                let id = st.get("id").and_then(Value::as_str).unwrap_or("?");
                let name = st.get("name").and_then(Value::as_str).unwrap_or("").replace('|', "/");
                let path = st.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                let path = if path.is_empty() { "(launch)".to_string() } else { path };
                match st.get("skipped").and_then(Value::as_str) {
                    Some(why) => s.push_str(&format!("| {id} | {name} | {path} | | left alone (matches \"{why}\"): off limits, don't rebuild it |\n")),
                    None => s.push_str(&format!(
                        "| {id} | {name} | {path} | {} | map/states/{id}/ |\n",
                        st.get("focused_title").and_then(Value::as_str).unwrap_or("").replace('|', "/")
                    )),
                }
            }
            let limits = &m["limits"];
            if let Some(by) = limits.get("stopped_by").and_then(Value::as_str) {
                s.push_str(&format!(
                    "\nThe map {}: {} key press(es) in {} s.{}\n",
                    if by == "exhausted" { "is complete".to_string() } else { format!("stopped at {by}") },
                    m.get("presses").and_then(Value::as_u64).unwrap_or(0),
                    m.get("seconds").and_then(Value::as_u64).unwrap_or(0),
                    if by == "exhausted" { "" } else { " Screens beyond it aren't listed; adb_ui_walkthrough_record from a kept screen reaches further." }
                ));
            }
        }
        None => s.push_str(&format!(
            "Not mapped: {}. Map them with adb_app_map into capture/map/ (screens.json and a folder per screen) before building, or build from the request alone and say so.\n",
            map_note.unwrap_or("the map wasn't made")
        )),
    }
    s.push_str("\nA screen left alone is one the person asked not to look at: it has no files here, and the app you build doesn't need it. Never pull, decode or read anything of the original app; these notes and the map are all of it you get.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_readme_lists_the_device_the_screens_and_the_rules() {
        let device = json!({"device": {"ro.product.manufacturer": "onn", "ro.product.model": "onn 4K", "ro.product.device": "sti6140", "ro.build.version.release": "14", "ro.build.version.sdk": "34"}, "tv": true, "density_dpi": 320, "display_px": {"width": 1920, "height": 1080}});
        let screens = json!({"keys": ["DPAD_RIGHT", "BACK"], "presses": 42, "seconds": 300, "limits": {"stopped_by": "max_states"}, "states": [
            {"id": "s01", "name": "Guide", "path": [], "focused_title": "Channel 1"},
            {"id": "s02", "name": "About | Info", "path": ["DPAD_RIGHT", "DPAD_CENTER"], "skipped": "About"},
        ]});
        let r = readme("com.example.tv", "192.168.1.20:5555", &device, Some(&screens), None);
        assert!(r.contains("# com.example.tv on 192.168.1.20:5555"));
        assert!(r.contains("a TV, driven by a remote"));
        assert!(r.contains("1920×1080 px at 320 dpi, that is 960×540 dp"));
        assert!(r.contains("| s01 | Guide | (launch) | Channel 1 | map/states/s01/ |"));
        assert!(r.contains("| s02 | About / Info | DPAD_RIGHT DPAD_CENTER | | left alone (matches \"About\")"));
        assert!(r.contains("2 screen(s)") && r.contains("1 kept, 1 left alone"));
        assert!(r.contains("stopped at max_states: 42 key press(es) in 300 s"));
        assert!(r.contains("Never pull, decode or read"));

        let none = readme("com.example.tv", "x", &json!({"error": "no device"}), None, Some("the person chose not to map it"));
        assert!(none.contains("The device didn't answer: no device"));
        assert!(none.contains("Not mapped: the person chose not to map it"));
    }
}
