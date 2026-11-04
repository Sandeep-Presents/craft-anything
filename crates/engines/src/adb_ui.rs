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

/// What the focused control says: its own label, else the labels of the controls inside it (a
/// Compose button is a group with no text of its own and a text child), else its identifier.
/// None without a focused control; empty when it has no words at all.
pub fn focus_title(nodes: &[UiNode]) -> Option<String> {
    let (i, f) = nodes.iter().enumerate().find(|(_, n)| n.focused)?;
    if !f.label().is_empty() {
        return Some(f.label().to_string());
    }
    let mut inside = String::new();
    for n in nodes.iter().skip(i + 1).take_while(|n| n.depth > f.depth) {
        let l = n.label().trim();
        if !l.is_empty() {
            if !inside.is_empty() {
                inside.push(' ');
            }
            inside.push_str(l);
            if inside.len() > 80 {
                break;
            }
        }
    }
    Some(if inside.is_empty() { f.resource_id.clone() } else { inside })
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
    let focused_title = focus_title(nodes);
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

/// A short fingerprint of a screen: its structure (depth, class and id of each control, with a
/// run of identical siblings counted once, so a list with more or fewer rows is the same
/// screen) and the texts of the controls a person can land on (digits read as one, so a clock
/// or a counter doesn't make a new screen). Bounds, focus and plain text content are left out:
/// a scrolled guide, a moved focus or a new minute on the clock is still the same screen; a
/// different set of buttons or menu entries is not.
pub fn screen_fingerprint(nodes: &[UiNode]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut last: Option<(usize, &str, &str, String)> = None;
    for n in nodes {
        let text = if n.focusable || n.clickable { digits_as_one(n.label()) } else { String::new() };
        let key = (n.depth, n.class.as_str(), n.resource_id.as_str(), text);
        if last.as_ref() == Some(&key) {
            continue;
        }
        key.hash(&mut h);
        last = Some(key);
    }
    format!("{:016x}", h.finish())
}

/// A label with every run of digits read as one (`50:28` → `#:#`, `Channel 23` → `Channel #`).
pub fn digits_as_one(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut in_digits = false;
    for c in label.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
                in_digits = true;
            }
        } else {
            out.push(c);
            in_digits = false;
        }
    }
    out
}

/// Most states one map keeps.
pub const MAX_MAP_STATES: usize = 60;

/// The texts a person reads on a screen: every non-empty label, in document order, each once.
pub fn visible_texts(nodes: &[UiNode]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    nodes.iter().filter(|n| n.visible).map(|n| n.label().trim()).filter(|t| !t.is_empty()).filter(|t| seen.insert(t.to_string())).map(str::to_string).collect()
}

/// The first of `skip_texts` that a label on screen contains (case-insensitive), when the screen
/// is one the person said to leave alone.
pub fn matches_skip(nodes: &[UiNode], skip_texts: &[String]) -> Option<String> {
    let wanted: Vec<(String, &str)> = skip_texts.iter().map(|t| (t.trim().to_lowercase(), t.as_str())).filter(|(l, _)| !l.is_empty()).collect();
    if wanted.is_empty() {
        return None;
    }
    for n in nodes.iter().filter(|n| n.visible) {
        let label = n.label().to_lowercase();
        if label.is_empty() {
            continue;
        }
        if let Some((_, original)) = wanted.iter().find(|(l, _)| label.contains(l.as_str())) {
            return Some((*original).to_string());
        }
    }
    None
}

/// A short name for a screen: the focused control's label, else the longest label on it, else
/// the first control's class; at most 60 characters.
pub fn state_name(nodes: &[UiNode]) -> String {
    let wordy = |l: &str| l.chars().filter(|c| c.is_alphabetic()).count() >= 2;
    let focused = nodes.iter().find(|n| n.focused).map(|n| n.label()).filter(|l| wordy(l));
    let control = nodes.iter().filter(|n| n.visible && (n.focusable || n.clickable)).map(|n| n.label()).find(|l| wordy(l));
    let longest = nodes.iter().filter(|n| n.visible).map(|n| n.label()).filter(|l| wordy(l)).max_by_key(|l| l.chars().count());
    let class = nodes.iter().find(|n| n.focused).or(nodes.first()).map(|n| n.class.rsplit('.').next().unwrap_or("screen"));
    let name = focused.or(control).or(longest).or(class).unwrap_or("screen");
    name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect()
}

/// A file-name-safe form of a name: lower case, letters and digits joined by `-`, at most 40
/// characters, `screen` when nothing is left.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "screen".to_string() } else { out }
}

/// One screen of a mapped app.
#[derive(Debug, Clone, Serialize)]
pub struct MapState {
    /// `s01`, `s02`… in the order found.
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    /// The keys from the app's launch that reach it.
    pub path: Vec<String>,
    pub parent: Option<String>,
    /// The key pressed on the parent to get here.
    pub key: Option<String>,
    /// The skip text that matched: the screen is a row with no details.
    pub skipped: Option<String>,
    pub focused_title: Option<String>,
    /// Controls read on it (0 for a skipped one).
    pub count: usize,
    /// Where its dump and picture are, relative to the map folder (none when skipped).
    pub files: Option<Value>,
    /// The focus positions walked on it, the landing one first: the keys that move the focus
    /// and OK are pressed from each, so a control's own screen is found.
    pub focuses: Vec<MapFocus>,
}

/// One focus position on a screen and the keys from the launch that put the focus there.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MapFocus {
    pub title: String,
    pub path: Vec<String>,
}

/// A key that left the app: what was on screen belonged to another package.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MapExit {
    pub from: String,
    pub key: String,
    pub package: String,
}

/// The keys pressed from each further focus position of a known screen: the ones that act on
/// the focused control or move the focus on. BACK and MENU act on the screen and were pressed
/// from its first position already.
pub const FOCUS_KEYS: [&str; 6] = ["DPAD_UP", "DPAD_DOWN", "DPAD_LEFT", "DPAD_RIGHT", "DPAD_CENTER", "ENTER"];

/// Most focus positions walked on one screen.
pub const MAX_MAP_FOCUS: usize = 30;

/// The package whose controls fill the screen: the most common one among the nodes, leaving
/// out the system bars. None when the dump has no package at all.
pub fn foreground_package(nodes: &[UiNode]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for n in nodes.iter().filter(|n| !n.package.is_empty() && n.package != "com.android.systemui") {
        *counts.entry(n.package.as_str()).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(p, _)| p.to_string())
}

/// A key pressed on one screen and where it led.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MapEdge {
    pub from: String,
    pub key: String,
    pub to: String,
}

/// The screens of an app and the keys between them, built breadth first. Pure: the caller
/// drives the device and brings each capture here.
#[derive(Debug, Default)]
pub struct ScreenMap {
    states: Vec<MapState>,
    edges: Vec<MapEdge>,
    exits: Vec<MapExit>,
    by_fingerprint: std::collections::BTreeMap<String, String>,
}

impl ScreenMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub fn states(&self) -> &[MapState] {
        &self.states
    }

    pub fn get(&self, id: &str) -> Option<&MapState> {
        self.states.iter().find(|s| s.id == id)
    }

    /// The state a capture belongs to: the one already known by its fingerprint, else a new one
    /// made by `make` with the next id. Returns the id and whether it is new. A key from `from`
    /// records an edge either way.
    pub fn arrive(&mut self, from: Option<&str>, key: Option<&str>, fingerprint: &str, make: impl FnOnce(String) -> MapState) -> (String, bool) {
        let (id, new) = match self.by_fingerprint.get(fingerprint) {
            Some(id) => (id.clone(), false),
            None => {
                let id = format!("s{:02}", self.states.len() + 1);
                let mut state = make(id.clone());
                state.id = id.clone();
                state.fingerprint = fingerprint.to_string();
                self.states.push(state);
                self.by_fingerprint.insert(fingerprint.to_string(), id.clone());
                (id, true)
            }
        };
        if let (Some(from), Some(key)) = (from, key) {
            let edge = MapEdge { from: from.to_string(), key: key.to_string(), to: id.clone() };
            if !self.edges.contains(&edge) {
                self.edges.push(edge);
            }
        }
        (id, new)
    }

    /// A key from `from` that left the app for `package`: no state, an exit. Kept once.
    pub fn leave(&mut self, from: &str, key: &str, package: &str) {
        let exit = MapExit { from: from.to_string(), key: key.to_string(), package: package.to_string() };
        if !self.exits.contains(&exit) {
            self.exits.push(exit);
        }
    }

    pub fn exits(&self) -> &[MapExit] {
        &self.exits
    }

    /// A focus position seen on screen `id`, reached by `path`: true when it is new there and
    /// the screen has fewer than `max` so far (so it is worth pressing keys from).
    pub fn note_focus(&mut self, id: &str, title: &str, path: &[String], max: usize) -> bool {
        let Some(s) = self.states.iter_mut().find(|s| s.id == id) else { return false };
        if s.focuses.iter().any(|f| f.title == title) || s.focuses.len() >= max {
            return false;
        }
        s.focuses.push(MapFocus { title: title.to_string(), path: path.to_vec() });
        true
    }

    /// Fill in a new state's files once its folder has its final name.
    pub fn set_files(&mut self, id: &str, files: Value) {
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            s.files = Some(files);
        }
    }

    /// The map as `screens.json`: `meta` fields first (device, package, limits), then the states
    /// and edges.
    pub fn to_json(&self, meta: Value) -> Value {
        let mut out = json!({"version": 1});
        if let (Some(o), Some(m)) = (out.as_object_mut(), meta.as_object()) {
            for (k, v) in m {
                o.insert(k.clone(), v.clone());
            }
            o.insert("count".into(), json!(self.states.len()));
            o.insert("skipped".into(), json!(self.states.iter().filter(|s| s.skipped.is_some()).count()));
            o.insert("states".into(), json!(self.states));
            o.insert("edges".into(), json!(self.edges));
            o.insert("exits".into(), json!(self.exits));
        }
        out
    }
}

#[cfg(test)]
mod map_tests {
    use super::*;

    fn screen(labels: &[&str], focused: usize) -> Vec<UiNode> {
        labels
            .iter()
            .enumerate()
            .map(|(i, l)| UiNode {
                index: i,
                class: "android.widget.TextView".into(),
                text: (*l).into(),
                focused: i == focused,
                focusable: true,
                visible: true,
                enabled: true,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn names_texts_skips_and_slugs() {
        let s = screen(&["Guide", "Settings", "A much longer label here"], 1);
        assert_eq!(state_name(&s), "Settings");
        assert_eq!(state_name(&screen(&["x", "A much longer label"], 9)), "A much longer label");
        assert_eq!(state_name(&screen(&["50:28", "Guide"], 0)), "Guide", "a clock in focus isn't a name; a control with words is");
        assert_eq!(state_name(&screen(&["50:28", "1", "2"], 0)), "TextView", "nothing wordy: the focused control's class");
        assert_eq!(state_name(&[]), "screen");
        assert_eq!(visible_texts(&screen(&["a", "b", "a", ""], 0)), ["a", "b"]);
        assert_eq!(matches_skip(&s, &["about".into(), "SETTINGS".into()]), Some("SETTINGS".into()));
        assert_eq!(matches_skip(&s, &["About".into(), "  ".into()]), None);
        assert_eq!(matches_skip(&s, &[]), None);
        assert_eq!(slug("All movies / shows!"), "all-movies-shows");
        assert_eq!(slug("***"), "screen");
        let long = slug(&"ab ".repeat(40));
        assert!(long.len() <= 40 && !long.ends_with('-'), "{long}");
    }

    #[test]
    fn a_map_grows_breadth_first_and_folds_known_screens() {
        let mut map = ScreenMap::new();
        let mk = |name: &str| {
            let name = name.to_string();
            move |id: String| MapState {
                id,
                name,
                fingerprint: String::new(),
                path: vec![],
                parent: None,
                key: None,
                skipped: None,
                focused_title: None,
                count: 1,
                files: None,
                focuses: vec![],
            }
        };
        let (home, new) = map.arrive(None, None, "fp-home", mk("Home"));
        assert!(new && home == "s01");
        let (guide, new) = map.arrive(Some("s01"), Some("DPAD_RIGHT"), "fp-guide", mk("Guide"));
        assert!(new && guide == "s02");
        // BACK from the guide leads to a screen already known: no new state, one more edge.
        let (back, new) = map.arrive(Some("s02"), Some("BACK"), "fp-home", mk("Home again"));
        assert!(!new && back == "s01");
        assert_eq!(map.len(), 2);
        // The same edge twice is kept once.
        map.arrive(Some("s02"), Some("BACK"), "fp-home", mk("Home again"));
        let json = map.to_json(json!({"package": "com.example.tv"}));
        assert_eq!(json["count"], 2);
        assert_eq!(json["edges"].as_array().unwrap().len(), 2);
        assert_eq!(json["package"], "com.example.tv");
        assert_eq!(json["states"][1]["name"], "Guide");
        map.set_files("s02", json!({"ui": "states/s02/ui.json"}));
        assert_eq!(map.get("s02").unwrap().files.as_ref().unwrap()["ui"], "states/s02/ui.json");
        assert!(map.get("s09").is_none());
    }

    #[test]
    fn a_key_that_leaves_the_app_is_an_exit_and_focus_positions_are_walked_once_each_up_to_a_cap() {
        let mut map = ScreenMap::new();
        let mk = move |id: String| MapState {
            id,
            name: "Home".into(),
            fingerprint: String::new(),
            path: vec![],
            parent: None,
            key: None,
            skipped: None,
            focused_title: None,
            count: 3,
            files: None,
            focuses: vec![],
        };
        map.arrive(None, None, "fp-home", mk);
        // BACK on the home screen showed the TV's launcher: an exit, not a screen, kept once.
        map.leave("s01", "BACK", "com.android.tvlauncher");
        map.leave("s01", "BACK", "com.android.tvlauncher");
        assert_eq!(map.len(), 1);
        assert_eq!(map.exits().len(), 1);
        // The focus walked down the home screen: each new position once, bounded.
        assert!(map.note_focus("s01", "Channels", &["DPAD_DOWN".into()], 2));
        assert!(!map.note_focus("s01", "Channels", &["DPAD_DOWN".into(), "DPAD_UP".into(), "DPAD_DOWN".into()], 2), "seen already");
        assert!(map.note_focus("s01", "Settings", &["DPAD_DOWN".into(), "DPAD_DOWN".into()], 2));
        assert!(!map.note_focus("s01", "About", &["DPAD_DOWN".into(), "DPAD_DOWN".into(), "DPAD_DOWN".into()], 2), "at the cap");
        assert!(!map.note_focus("s07", "About", &[], 2), "no such screen");
        let json = map.to_json(json!({}));
        assert_eq!(json["exits"][0], json!({"from": "s01", "key": "BACK", "package": "com.android.tvlauncher"}));
        assert_eq!(json["states"][0]["focuses"][1], json!({"title": "Settings", "path": ["DPAD_DOWN", "DPAD_DOWN"]}));
    }

    #[test]
    fn the_focus_is_named_by_the_words_inside_a_wordless_control() {
        // A Compose button: a focused group with no text, its text in a child.
        let mut nodes = screen(&["Trial home", "", "Channels", "", "Settings"], 1);
        nodes[1].depth = 1;
        nodes[2].depth = 2;
        nodes[3].depth = 1;
        nodes[4].depth = 2;
        assert_eq!(focus_title(&nodes).as_deref(), Some("Channels"), "the child's text, not the sibling's");
        nodes[1].focused = false;
        nodes[3].focused = true;
        assert_eq!(focus_title(&nodes).as_deref(), Some("Settings"));
        let mut own = screen(&["Guide", "x"], 0);
        own[1].depth = 1;
        assert_eq!(focus_title(&own).as_deref(), Some("Guide"), "its own label first");
        let mut bare = screen(&["", ""], 0);
        bare[0].resource_id = "app:id/row".into();
        assert_eq!(focus_title(&bare).as_deref(), Some("app:id/row"), "an identifier when nothing has words");
        assert_eq!(focus_title(&screen(&["a"], 9)), None, "nothing focused");
        assert_eq!(ui_tree_json(&nodes, None, None)["focused_title"], "Settings");
    }

    #[test]
    fn the_foreground_package_is_the_one_most_controls_belong_to_without_the_system_bars() {
        let mut nodes = screen(&["Home", "Apps", "Search"], 0);
        for n in &mut nodes {
            n.package = "com.android.tvlauncher".into();
        }
        nodes.push(UiNode { package: "com.android.systemui".into(), ..Default::default() });
        nodes.push(UiNode { package: "com.android.systemui".into(), ..Default::default() });
        nodes.push(UiNode { package: "com.android.systemui".into(), ..Default::default() });
        nodes.push(UiNode { package: "com.android.systemui".into(), ..Default::default() });
        assert_eq!(foreground_package(&nodes).as_deref(), Some("com.android.tvlauncher"));
        assert_eq!(foreground_package(&screen(&["x"], 0)), None, "no package in the dump");
        assert_eq!(foreground_package(&[]), None);
    }
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
    fn a_fingerprint_ignores_bounds_focus_list_length_plain_text_and_digits() {
        let a = parse_ui_dump(DUMP, 100);
        let moved = DUMP.replace("[144,502][584,579]", "[144,600][584,677]").replace("focused=\"true\"", "focused=\"false\"");
        let b = parse_ui_dump(&moved, 100);
        assert_eq!(screen_fingerprint(&a), screen_fingerprint(&b));
        // The text of a control a person can land on is part of the screen.
        let other = DUMP.replace("All movies", "All shows");
        assert_ne!(screen_fingerprint(&a), screen_fingerprint(&parse_ui_dump(&other, 100)));
        // Rows of a list: more of the same rows is the same screen; a clock is the same screen.
        let node = |cls: &str, id: &str, t: &str, focusable: bool| {
            format!(
                "<node index=\"0\" text=\"{t}\" resource-id=\"{id}\" class=\"{cls}\" package=\"p\" content-desc=\"\" checkable=\"false\" checked=\"false\" clickable=\"{focusable}\" enabled=\"true\" focusable=\"{focusable}\" focused=\"false\" scrollable=\"false\" long-clickable=\"false\" password=\"false\" selected=\"false\" visible-to-user=\"true\" bounds=\"[0,0][100,20]\" />"
            )
        };
        let screen = |rows: &[&str], time: &str, btn: &str| {
            let rows: String = rows.iter().map(|r| node("android.widget.TextView", "app:id/row", r, false)).collect();
            format!(
                "<hierarchy>{}{rows}{}</hierarchy>",
                node("android.widget.TextView", "app:id/clock", time, false),
                node("android.widget.Button", "app:id/go", btn, true)
            )
        };
        let three = parse_ui_dump(&screen(&["News", "Sports", "Movies"], "7:49 PM", "Channel 23"), 100);
        let five = parse_ui_dump(&screen(&["Kids", "Docs", "Music", "News", "Sports"], "7:50 PM", "Channel 24"), 100);
        assert_eq!(screen_fingerprint(&three), screen_fingerprint(&five), "a scrolled list and a new minute are the same screen");
        let other_button = parse_ui_dump(&screen(&["News"], "7:49 PM", "Settings"), 100);
        assert_ne!(screen_fingerprint(&three), screen_fingerprint(&other_button), "another control is another screen");
        assert_eq!(digits_as_one("50:28 · Channel 23"), "#:# · Channel #");
        assert_eq!(digits_as_one("abc"), "abc");
    }
}
