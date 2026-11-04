//! An Android device's screen as a person sees and drives it: the keys a remote or keyboard
//! sends (`input keyevent`), typed text (`input text`), the display's size and density (`wm`),
//! and the screen's controls as `uiautomator dump` reports them.
//!
//! Everything here is pure: validation of what may reach the device shell, and bounded parsers
//! of what came back. Nothing runs; [`super::adb`] does that. Nothing reads inside the app: the
//! accessibility tree and the keys are what any user of the device sees and does, so the tools
//! built on this module belong to Clean room.

use craft_anything_evidence::ToolError;
use serde::Serialize;
use serde_json::{Value, json};

/// Most keys one call may send, after repeats are expanded.
pub const MAX_KEYS: usize = 200;
/// Most times one key token may repeat (`DPAD_DOWN x50`).
pub const MAX_REPEAT: u32 = 50;
/// Longest text one call may type.
pub const MAX_TEXT: usize = 256;
/// Most nodes a UI dump parse keeps.
pub const MAX_UI_NODES: usize = 5_000;
/// Longest attribute value kept from a UI dump (text can be a whole article).
const MAX_ATTR: usize = 4_096;

/// The keys a person can send with a remote, a keyboard or a game pad, by their Android key
/// code name (without the `KEYCODE_` prefix). Power, sleep and the like are not here: nothing
/// in this list changes the device beyond what the app on screen does with it.
pub const KEYS: &[&str] = &[
    "DPAD_UP",
    "DPAD_DOWN",
    "DPAD_LEFT",
    "DPAD_RIGHT",
    "DPAD_CENTER",
    "ENTER",
    "BACK",
    "HOME",
    "MENU",
    "INFO",
    "GUIDE",
    "SEARCH",
    "SETTINGS",
    "CAPTIONS",
    "TV",
    "TV_INPUT",
    "CHANNEL_UP",
    "CHANNEL_DOWN",
    "LAST_CHANNEL",
    "MEDIA_PLAY",
    "MEDIA_PAUSE",
    "MEDIA_PLAY_PAUSE",
    "MEDIA_STOP",
    "MEDIA_NEXT",
    "MEDIA_PREVIOUS",
    "MEDIA_REWIND",
    "MEDIA_FAST_FORWARD",
    "MEDIA_RECORD",
    "VOLUME_UP",
    "VOLUME_DOWN",
    "VOLUME_MUTE",
    "PROG_RED",
    "PROG_GREEN",
    "PROG_YELLOW",
    "PROG_BLUE",
    "DEL",
    "FORWARD_DEL",
    "TAB",
    "SPACE",
    "ESCAPE",
    "MOVE_HOME",
    "MOVE_END",
    "PAGE_UP",
    "PAGE_DOWN",
    "STAR",
    "POUND",
    "MINUS",
    "PLUS",
    "EQUALS",
    "COMMA",
    "PERIOD",
    "SLASH",
    "AT",
    "BUTTON_A",
    "BUTTON_B",
    "BUTTON_X",
    "BUTTON_Y",
    "BUTTON_L1",
    "BUTTON_R1",
    "BUTTON_SELECT",
    "BUTTON_START",
];

/// One key to send, `times` over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyPress {
    /// The key code name without `KEYCODE_` (`DPAD_RIGHT`).
    pub key: String,
    pub times: u32,
}

/// A key name as a person writes it: `DPAD_RIGHT`, `dpad_right`, `KEYCODE_DPAD_RIGHT`, a digit
/// or a letter. Anything else is refused, so only allowlisted names reach `input keyevent`.
pub fn validate_key(s: &str) -> Result<String, ToolError> {
    let name = s.trim().to_ascii_uppercase();
    let name = name.strip_prefix("KEYCODE_").unwrap_or(&name).to_string();
    let single = name.len() == 1 && name.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase());
    if single || KEYS.contains(&name.as_str()) {
        Ok(name)
    } else {
        Err(ToolError::invalid(format!("not a key this tool sends: {s:?}")).with_remediation(
            "Use a key name such as DPAD_RIGHT, DPAD_CENTER, BACK, MENU, INFO, CHANNEL_UP, MEDIA_PLAY_PAUSE, a digit or a letter; add ` x3` to repeat it.",
        ))
    }
}

/// Key tokens as a person writes them (`"DPAD_RIGHT"`, `"DPAD_DOWN x3"`, `"OK"` for
/// DPAD_CENTER), expanded and bounded: at most [`MAX_REPEAT`] per token, [`MAX_KEYS`] in all.
pub fn parse_keys(tokens: &[String]) -> Result<Vec<KeyPress>, ToolError> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut total: usize = 0;
    for token in tokens {
        let token = token.trim();
        // `DPAD_DOWN x3`, `DPAD_DOWN*3` or `DPAD_DOWN 3`; anything else after the name is refused.
        let (name, times) = match token.split_once(|c: char| c.is_whitespace() || c == '*') {
            Some((name, rep)) => {
                let digits = rep.trim().trim_start_matches(['x', 'X']);
                let times = if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { 0 } else { digits.parse::<u32>().unwrap_or(0) };
                (name.trim(), times)
            }
            None => (token, 1),
        };
        if times == 0 || times > MAX_REPEAT {
            return Err(ToolError::invalid(format!("a key repeats between 1 and {MAX_REPEAT} times, not {times} ({token:?})")));
        }
        let key = match name.to_ascii_uppercase().as_str() {
            "OK" | "SELECT" | "CENTER" => "DPAD_CENTER".to_string(),
            "UP" | "DOWN" | "LEFT" | "RIGHT" => format!("DPAD_{}", name.to_ascii_uppercase()),
            "PLAY" => "MEDIA_PLAY".to_string(),
            "PAUSE" => "MEDIA_PAUSE".to_string(),
            "PLAY_PAUSE" => "MEDIA_PLAY_PAUSE".to_string(),
            _ => validate_key(name)?,
        };
        total = total.saturating_add(times as usize);
        if total > MAX_KEYS {
            return Err(ToolError::invalid(format!("at most {MAX_KEYS} keys in one call"))
                .with_remediation("Send the keys in a few calls, or map the screens with adb_app_map."));
        }
        out.push(KeyPress { key, times });
    }
    if out.is_empty() {
        return Err(ToolError::invalid("no keys given"));
    }
    Ok(out)
}

/// The device-shell commands that send `keys` with `gap_ms` between presses: `input keyevent`
/// once per press, with a `sleep` between. Only validated key names reach the string.
pub fn key_commands(keys: &[KeyPress], gap_ms: u32) -> Vec<String> {
    let mut cmds = Vec::new();
    let gap = format!("sleep {}", seconds(gap_ms));
    for k in keys {
        for _ in 0..k.times {
            if !cmds.is_empty() && gap_ms > 0 {
                cmds.push(gap.clone());
            }
            cmds.push(format!("input keyevent KEYCODE_{}", k.key));
        }
    }
    cmds
}

/// Milliseconds as the decimal seconds `sleep` takes (`250` → `0.250`).
pub fn seconds(ms: u32) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Text a person types: printable ASCII, no `%` (which `input text` reads as an escape) and no
/// control characters, at most [`MAX_TEXT`] characters.
pub fn validate_text(s: &str) -> Result<&str, ToolError> {
    let ok = !s.is_empty() && s.chars().count() <= MAX_TEXT && s.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'%');
    if ok {
        Ok(s)
    } else {
        Err(ToolError::invalid(format!("text to type must be 1 to {MAX_TEXT} printable ASCII characters without %: {s:?}"))
            .with_remediation("Type plain words, numbers and punctuation; send other keys with adb_keys_press."))
    }
}

/// The device-shell command that types `text` (already validated): spaces become `%s`, the
/// whole thing single-quoted for the device shell.
pub fn text_command(text: &str) -> String {
    format!("input text {}", super::adb::quote_device(&text.replace(' ', "%s")))
}

/// The density `wm density` reports, the override when there is one (`Override density: 213`),
/// else the physical one.
pub fn parse_wm_density(text: &str) -> Option<u32> {
    parse_wm_value(text, "density").and_then(|v| v.parse::<u32>().ok()).filter(|d| (60..=1200).contains(d))
}

/// The size `wm size` reports as (width, height) in pixels, the override when there is one.
pub fn parse_wm_size(text: &str) -> Option<(u32, u32)> {
    let v = parse_wm_value(text, "size")?;
    let (w, h) = v.split_once('x')?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    ((1..=32_768).contains(&w) && (1..=32_768).contains(&h)).then_some((w, h))
}

fn parse_wm_value(text: &str, what: &str) -> Option<String> {
    let mut physical = None;
    for line in text.lines().take(64) {
        let line = line.trim();
        let Some((label, value)) = line.split_once(':') else { continue };
        let label = label.trim().to_ascii_lowercase();
        if label == format!("override {what}") {
            return Some(value.trim().to_string());
        }
        if label == format!("physical {what}") {
            physical = Some(value.trim().to_string());
        }
    }
    physical
}

/// A control's place on the screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Bounds {
    /// `[left,top][right,bottom]` as `uiautomator` writes it.
    pub fn parse(s: &str) -> Option<Bounds> {
        let s = s.trim();
        let inner = s.strip_prefix('[')?.strip_suffix(']')?;
        let (lt, rb) = inner.split_once("][")?;
        let (l, t) = lt.split_once(',')?;
        let (r, b) = rb.split_once(',')?;
        let n = |v: &str| v.trim().parse::<i32>().ok().filter(|v| (-100_000..=100_000).contains(v));
        let (l, t, r, b) = (n(l)?, n(t)?, n(r)?, n(b)?);
        Some(Bounds { x: l, y: t, width: r.saturating_sub(l), height: b.saturating_sub(t) })
    }

    /// The same bounds in density-independent pixels (rounded to the nearest dp).
    pub fn to_dp(self, density_dpi: u32) -> Bounds {
        let f = |v: i32| ((v as f64) * 160.0 / (density_dpi.max(1) as f64)).round() as i32;
        Bounds { x: f(self.x), y: f(self.y), width: f(self.width), height: f(self.height) }
    }
}

/// One control from a `uiautomator dump`, in document order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UiNode {
    /// Position in document order (0-based).
    pub index: usize,
    /// Nesting depth below the hierarchy root.
    pub depth: usize,
    /// The view class (`android.widget.TextView`).
    pub class: String,
    pub package: String,
    pub resource_id: String,
    pub text: String,
    pub content_desc: String,
    pub bounds: Option<Bounds>,
    pub focused: bool,
    pub focusable: bool,
    pub selected: bool,
    pub clickable: bool,
    pub long_clickable: bool,
    pub scrollable: bool,
    pub checkable: bool,
    pub checked: bool,
    pub enabled: bool,
    pub visible: bool,
    /// A password field: its text is never kept.
    pub password: bool,
}

impl UiNode {
    /// What a person reads on the control: its text, else its description.
    pub fn label(&self) -> &str {
        if !self.text.is_empty() { &self.text } else { &self.content_desc }
    }

    /// The control's role in the words the window tools use (`button`, `checkbox`, `edit`,
    /// `text`…): from its class, or `button` for a generic view a person can press or focus.
    pub fn role(&self) -> String {
        let short = self.class.rsplit('.').next().unwrap_or(&self.class);
        let role = match short {
            "Button" | "ImageButton" | "MaterialButton" => "button",
            "CheckBox" | "CheckedTextView" => "checkbox",
            "RadioButton" => "radiobutton",
            "Switch" | "SwitchCompat" | "ToggleButton" => "togglebutton",
            "EditText" | "AutoCompleteTextView" | "TextInputEditText" => "edit",
            "Spinner" => "combobox",
            "SeekBar" | "Slider" => "slider",
            "TextView" => "text",
            "ImageView" => "image",
            "RecyclerView" | "ListView" | "GridView" | "ScrollView" | "HorizontalScrollView" => "list",
            "ViewGroup" | "FrameLayout" | "LinearLayout" | "RelativeLayout" | "ConstraintLayout" | "View" | "ComposeView" => "",
            other => return other.to_ascii_lowercase(),
        };
        if role.is_empty() {
            if (self.clickable || self.focusable) && !self.label().is_empty() { "button".to_string() } else { "group".to_string() }
        } else {
            role.to_string()
        }
    }
}

/// The nodes of a `uiautomator dump`, in document order, at most `max_nodes` of them. Hostile or
/// cut-off XML ends the parse where it stops making sense; what was read is returned.
pub fn parse_ui_dump(xml: &str, max_nodes: usize) -> Vec<UiNode> {
    let max_nodes = max_nodes.min(MAX_UI_NODES);
    let mut nodes = Vec::new();
    let mut depth: usize = 0;
    let mut rest = xml;
    while nodes.len() < max_nodes {
        let Some(lt) = rest.find('<') else { break };
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("</node") {
            depth = depth.saturating_sub(1);
            let Some(gt) = after.find('>') else { break };
            rest = &after[gt + 1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<node") {
            let Some((node, self_closing, used)) = parse_node(after, nodes.len(), depth) else { break };
            nodes.push(node);
            if !self_closing {
                depth = depth.saturating_add(1);
            }
            rest = &after[used..];
            continue;
        }
        // Another tag (`<?xml`, `<hierarchy`, a comment): skip past its end.
        let Some(gt) = rest.find('>') else { break };
        rest = &rest[gt + 1..];
    }
    nodes
}

/// The attributes of one `<node …>` after its name: the node, whether the tag self-closed, and
/// how many bytes were read (through the `>`).
fn parse_node(s: &str, index: usize, depth: usize) -> Option<(UiNode, bool, usize)> {
    let mut node = UiNode { index, depth, enabled: true, visible: true, ..Default::default() };
    let bytes = s.as_bytes();
    let mut i = 0;
    loop {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => return None,
            Some(b'>') => return Some((node, false, i + 1)),
            Some(b'/') => {
                return if bytes.get(i + 1) == Some(&b'>') { Some((node, true, i + 2)) } else { None };
            }
            _ => {}
        }
        // name="value"
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b':')) {
            i += 1;
        }
        let name = s.get(name_start..i)?;
        if name.is_empty() || bytes.get(i) != Some(&b'=') {
            return None;
        }
        i += 1;
        let quote = *bytes.get(i)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        let raw = s.get(value_start..i)?;
        i += 1; // closing quote
        let value = decode_entities(raw);
        let value: String = value.chars().take(MAX_ATTR).collect();
        let flag = value == "true";
        match name {
            "class" => node.class = value,
            "package" => node.package = value,
            "resource-id" => node.resource_id = value,
            "text" => node.text = value,
            "content-desc" => node.content_desc = value,
            "bounds" => node.bounds = Bounds::parse(&value),
            "focused" => node.focused = flag,
            "focusable" => node.focusable = flag,
            "selected" => node.selected = flag,
            "clickable" => node.clickable = flag,
            "long-clickable" => node.long_clickable = flag,
            "scrollable" => node.scrollable = flag,
            "checkable" => node.checkable = flag,
            "checked" => node.checked = flag,
            "enabled" => node.enabled = flag,
            "visible-to-user" => node.visible = flag,
            "password" => node.password = flag,
            _ => {}
        }
        if node.password {
            node.text.clear();
        }
    }
}

/// `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and numeric references, as XML writes them.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&n| n <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|n| if let Some(h) = n.strip_prefix(['x', 'X']) { u32::from_str_radix(h, 16).ok() } else { n.parse().ok() })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// The dump as the `ui-tree.json` the window tools write: `nodes` with `role`, `title`,
/// `identifier`, `value`, `depth`, `position` and `size` in pixels (and `dp` when the density is
/// known), plus what a remote cares about: `focused`, `focusable`, `selected`, `clickable`,
/// `scrollable`, `enabled`. `focused` at the top is the index of the focused node.
pub fn ui_tree_json(nodes: &[UiNode], density_dpi: Option<u32>, package: Option<&str>) -> Value {
    let rows: Vec<Value> = nodes
        .iter()
        .map(|n| {
            let mut row = json!({
                "index": n.index,
                "depth": n.depth,
                "role": n.role(),
                "class": n.class,
                "title": n.label(),
                "identifier": n.resource_id,
                "value": n.content_desc,
                "focused": n.focused,
                "focusable": n.focusable,
                "selected": n.selected,
                "clickable": n.clickable,
                "scrollable": n.scrollable,
                "enabled": n.enabled,
            });
            if let Some(b) = n.bounds {
                row["position"] = json!({"x": b.x, "y": b.y});
                row["size"] = json!({"width": b.width, "height": b.height});
                if let Some(d) = density_dpi {
                    let dp = b.to_dp(d);
                    row["dp"] = json!({"x": dp.x, "y": dp.y, "width": dp.width, "height": dp.height});
                }
            }
            row
        })
        .collect();
    let focused = nodes.iter().find(|n| n.focused).map(|n| n.index);
    let focused_title = nodes.iter().find(|n| n.focused).map(|n| n.label().to_string());
    json!({
        "version": 1,
        "source": "uiautomator",
        "package": package,
        "density_dpi": density_dpi,
        "count": rows.len(),
        "focused": focused,
        "focused_title": focused_title,
        "nodes": rows,
    })
}

/// A short fingerprint of a screen's structure (classes, ids and texts in order, bounds left
/// out), so two dumps of the same screen match and a scrolled list or a moved focus still does.
pub fn screen_fingerprint(nodes: &[UiNode]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for n in nodes {
        n.depth.hash(&mut h);
        n.class.hash(&mut h);
        n.resource_id.hash(&mut h);
        n.label().hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0"><node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="com.example.tv" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" visible-to-user="true" bounds="[0,0][1920,1080]"><node index="0" text="All movies" resource-id="com.example.tv:id/row" class="android.widget.TextView" package="com.example.tv" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="true" scrollable="false" long-clickable="false" password="false" selected="true" visible-to-user="true" bounds="[144,502][584,579]" /><node index="1" text="Tom &amp; Jerry &lt;3" resource-id="" class="android.view.View" package="com.example.tv" content-desc="Open &quot;Tom&quot;" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" visible-to-user="true" bounds="[600,502][1000,579]" /><node index="2" text="secret" resource-id="" class="android.widget.EditText" package="com.example.tv" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="true" selected="false" visible-to-user="true" bounds="[0,600][400,660]" /></node></hierarchy>"#;

    #[test]
    fn keys_are_allowlisted_expanded_and_bounded() {
        let keys = parse_keys(&["DPAD_RIGHT".into(), "down x3".into(), "OK".into(), "KEYCODE_BACK".into(), "7".into(), "a".into()]).unwrap();
        let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
        assert_eq!(names, ["DPAD_RIGHT", "DPAD_DOWN", "DPAD_CENTER", "BACK", "7", "A"]);
        assert_eq!(keys[1].times, 3);
        assert!(parse_keys(&["POWER".into()]).is_err(), "power is not a key a person sends to an app");
        assert!(parse_keys(&["DPAD_UP x0".into()]).is_err());
        assert!(parse_keys(&["DPAD_UP x51".into()]).is_err());
        assert!(parse_keys(&vec![String::from("DPAD_UP x50"); 5]).is_err(), "250 keys is over the cap");
        assert!(parse_keys(&[]).is_err());
        assert!(parse_keys(&["DPAD_UP; rm -rf /".into()]).is_err());
        let cmds = key_commands(&parse_keys(&["UP x2".into(), "OK".into()]).unwrap(), 250);
        assert_eq!(
            cmds,
            ["input keyevent KEYCODE_DPAD_UP", "sleep 0.250", "input keyevent KEYCODE_DPAD_UP", "sleep 0.250", "input keyevent KEYCODE_DPAD_CENTER"]
        );
        assert_eq!(key_commands(&parse_keys(&["OK x2".into()]).unwrap(), 0).len(), 2);
    }

    #[test]
    fn typed_text_is_plain_and_quoted() {
        assert_eq!(text_command(validate_text("hello world").unwrap()), "input text 'hello%sworld'");
        assert_eq!(text_command("it's"), "input text 'it'\\''s'");
        assert!(validate_text("").is_err());
        assert!(validate_text("100%").is_err());
        assert!(validate_text("a\nb").is_err());
        assert!(validate_text("héllo").is_err());
        assert!(validate_text(&"x".repeat(MAX_TEXT + 1)).is_err());
    }

    #[test]
    fn wm_density_and_size_prefer_the_override() {
        assert_eq!(parse_wm_density("Physical density: 320\nOverride density: 213\n"), Some(213));
        assert_eq!(parse_wm_density("Physical density: 320\n"), Some(320));
        assert_eq!(parse_wm_density("garbage"), None);
        assert_eq!(parse_wm_density("Physical density: 99999"), None);
        assert_eq!(parse_wm_size("Physical size: 1920x1080\n"), Some((1920, 1080)));
        assert_eq!(parse_wm_size("Physical size: 3840x2160\nOverride size: 1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_wm_size("Physical size: 1920"), None);
    }

    #[test]
    fn a_dump_becomes_nodes_with_depth_bounds_and_decoded_text() {
        let nodes = parse_ui_dump(DUMP, 100);
        assert_eq!(nodes.len(), 4);
        assert_eq!((nodes[0].depth, nodes[1].depth, nodes[3].depth), (0, 1, 1));
        let row = &nodes[1];
        assert_eq!(row.text, "All movies");
        assert_eq!(row.resource_id, "com.example.tv:id/row");
        assert!(row.focused && row.selected && row.focusable);
        assert_eq!(row.bounds, Some(Bounds { x: 144, y: 502, width: 440, height: 77 }));
        assert_eq!(row.bounds.unwrap().to_dp(320), Bounds { x: 72, y: 251, width: 220, height: 39 });
        assert_eq!(row.role(), "text");
        let view = &nodes[2];
        assert_eq!(view.text, "Tom & Jerry <3");
        assert_eq!(view.content_desc, "Open \"Tom\"");
        assert_eq!(view.role(), "button", "a generic view a person can press reads as a button");
        let secret = &nodes[3];
        assert!(secret.password && secret.text.is_empty(), "a password field's text is never kept");
        assert_eq!(secret.role(), "edit");
    }

    #[test]
    fn hostile_dumps_end_quietly() {
        assert!(parse_ui_dump("", 10).is_empty());
        assert!(parse_ui_dump("<node text=\"unterminated", 10).is_empty());
        assert_eq!(parse_ui_dump("<node a=\"1\"/><node b=\"2\"/><node c=\"3\"/>", 2).len(), 2);
        assert!(parse_ui_dump("<node =\"x\"/>", 10).is_empty());
        assert_eq!(parse_ui_dump("<node bounds=\"[1,2][3\"/>", 10)[0].bounds, None);
        let huge = format!("<node text=\"{}\"/>", "a".repeat(100_000));
        assert_eq!(parse_ui_dump(&huge, 10)[0].text.len(), MAX_ATTR);
        assert_eq!(decode_entities("a &#65;&#x42; &bogus; &"), "a AB &bogus; &");
    }

    #[test]
    fn the_tree_json_matches_the_window_tools_shape() {
        let nodes = parse_ui_dump(DUMP, 100);
        let tree = ui_tree_json(&nodes, Some(320), Some("com.example.tv"));
        assert_eq!(tree["count"], 4);
        assert_eq!(tree["focused"], 1);
        assert_eq!(tree["focused_title"], "All movies");
        let n = &tree["nodes"][1];
        assert_eq!(n["role"], "text");
        assert_eq!(n["title"], "All movies");
        assert_eq!(n["identifier"], "com.example.tv:id/row");
        assert_eq!(n["position"]["x"], 144);
        assert_eq!(n["size"]["width"], 440);
        assert_eq!(n["dp"]["y"], 251);
        let no_density = ui_tree_json(&nodes, None, None);
        assert!(no_density["nodes"][1].get("dp").is_none());
    }

    #[test]
    fn a_fingerprint_ignores_bounds_and_focus() {
        let a = parse_ui_dump(DUMP, 100);
        let moved = DUMP.replace("[144,502][584,579]", "[144,600][584,677]").replace("focused=\"true\"", "focused=\"false\"");
        let b = parse_ui_dump(&moved, 100);
        assert_eq!(screen_fingerprint(&a), screen_fingerprint(&b));
        let other = DUMP.replace("All movies", "All shows");
        assert_ne!(screen_fingerprint(&a), screen_fingerprint(&parse_ui_dump(&other, 100)));
    }
}
