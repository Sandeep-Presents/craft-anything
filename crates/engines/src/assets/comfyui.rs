//! Pictures from a ComfyUI server the person runs (on this computer or another): no key, no cost.
//! A picture is one API-format graph posted to `/prompt`, polled at `/history/{id}` until it is
//! done, then fetched from `/view`. The built-in graph is a plain checkpoint text-to-image; a
//! workflow file the person exported in API format can replace it (its `{{prompt}}`, `{{negative}}`,
//! `{{width}}`, `{{height}}`, `{{seed}}` and `{{checkpoint}}` placeholders are filled in).
//! [`check`] asks a server what it is and which checkpoints it has, for Settings.

use super::{AssetProvider, AssetRequest, Generated, Shape, fal};
use craft_anything_evidence::{ErrorCode, ToolError};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The provider id the asset table names.
pub const ID: &str = "comfyui";
/// What the sampler is told to avoid.
const NEGATIVE: &str = "blurry, low quality, watermark, text";
/// The largest workflow file read.
const MAX_WORKFLOW: u64 = 2 << 20;
/// The largest JSON answer kept.
const MAX_JSON: u64 = 8 << 20;
/// The largest picture kept.
const MAX_IMAGE: u64 = 100 << 20;
/// How long a picture may take, from the post to the last byte.
const DEADLINE: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_secs(1);

/// A ComfyUI server, as an [`AssetProvider`].
pub struct ComfyUi {
    address: String,
    checkpoint: String,
    workflow: Option<PathBuf>,
    poll: Duration,
    deadline: Duration,
}

impl ComfyUi {
    pub fn new(address: impl Into<String>, checkpoint: impl Into<String>, workflow: Option<PathBuf>) -> ComfyUi {
        ComfyUi { address: address.into(), checkpoint: checkpoint.into(), workflow, poll: POLL, deadline: DEADLINE }
    }
}

impl AssetProvider for ComfyUi {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "ComfyUI"
    }

    fn needs_key(&self) -> bool {
        false
    }

    fn generate(&self, _key: &str, req: &AssetRequest) -> Result<Generated, ToolError> {
        let AssetRequest::Image { prompt, shape, .. } = *req else {
            return Err(ToolError::new(ErrorCode::NotSupported, "ComfyUI makes pictures here, not 3D models.")
                .with_remediation("Choose a 3D model generator that takes a key, or make the model with simple shapes."));
        };
        let base = clean_address(&self.address)?;
        let checkpoint = self.checkpoint.trim();
        if checkpoint.is_empty() {
            return Err(no_checkpoint());
        }
        let (width, height) = size(shape);
        let vars = Vars { prompt, negative: NEGATIVE, width, height, seed: seed(), checkpoint };
        let graph = match &self.workflow {
            Some(path) => from_workflow(path, &vars)?,
            None => builtin_graph(&vars),
        };
        let started = Instant::now();
        let id = self.queue(&base, &graph)?;
        let image = self.wait_for(&base, &id, started)?;
        let bytes = self.view(&base, &image)?;
        Ok(Generated { url: fal::data_uri(&bytes, "image/png"), bytes, cost_usd: 0.0 })
    }
}

fn no_checkpoint() -> ToolError {
    ToolError::new(ErrorCode::BadSettings, "No ComfyUI checkpoint is chosen.")
        .with_remediation("Choose one in Settings → Builds → Picture model, then press Test.")
}

impl ComfyUi {
    fn queue(&self, base: &str, graph: &Value) -> Result<String, ToolError> {
        let body = json!({"prompt": graph, "client_id": "craft-anything"});
        let (status, text) =
            send(agent(Duration::from_secs(60)).post(&format!("{base}/prompt")).header("Content-Type", "application/json").send(body.to_string()), base)?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status == 400 || (200..300).contains(&status) && v.get("prompt_id").is_none() {
            return Err(refused(&v, &self.checkpoint));
        }
        if !(200..300).contains(&status) {
            return Err(http_error(base, status, &text));
        }
        v.get("prompt_id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| garbled("didn't say which job it queued"))
    }

    /// Poll the history until the job has an image, fails, or the deadline passes.
    fn wait_for(&self, base: &str, id: &str, started: Instant) -> Result<Image, ToolError> {
        let url = format!("{base}/history/{}", encode(id));
        loop {
            let (status, text) = send(agent(Duration::from_secs(30)).get(&url).call(), base)?;
            if !(200..300).contains(&status) {
                return Err(http_error(base, status, &text));
            }
            let v: Value = serde_json::from_str(&text).map_err(|_| garbled("sent an unreadable history"))?;
            if let Some(entry) = v.get(id) {
                if entry.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
                    return Err(failed(entry));
                }
                if let Some(image) = first_image(entry) {
                    return Ok(image);
                }
                if entry.pointer("/status/completed").and_then(Value::as_bool) == Some(true) {
                    return Err(ToolError::new(ErrorCode::ProgramFailed, "ComfyUI finished but made no picture.")
                        .with_remediation("The workflow needs a SaveImage (or PreviewImage) node; the built-in one has it."));
                }
            }
            if started.elapsed() >= self.deadline {
                return Err(ToolError::new(ErrorCode::TimedOut, "ComfyUI took too long to make the picture.")
                    .with_remediation("Check that the server is not stuck (a model still loading, a full queue), then try again."));
            }
            std::thread::sleep(self.poll);
        }
    }

    fn view(&self, base: &str, image: &Image) -> Result<Vec<u8>, ToolError> {
        let url = format!("{base}/view?filename={}&subfolder={}&type={}", encode(&image.filename), encode(&image.subfolder), encode(&image.kind));
        let resp = agent(Duration::from_secs(120)).get(&url).call().map_err(|e| unreachable_server(base, &e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(http_error(base, status, ""));
        }
        let bytes = read_capped(resp.into_body().into_reader(), MAX_IMAGE)?;
        if bytes.is_empty() {
            return Err(garbled("sent an empty picture"));
        }
        Ok(bytes)
    }
}

/// What a server is, for Settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComfyStatus {
    pub version: String,
    /// The graphics cards (or "CPU"), by name.
    pub devices: Vec<String>,
    /// The first device's memory in whole gigabytes, when the server says.
    pub vram_gb: Option<u64>,
    pub checkpoints: Vec<String>,
}

impl ComfyStatus {
    /// "ComfyUI 0.3.10 · NVIDIA GeForce RTX 3060 · 12 checkpoints" in English; Settings translates the parts.
    pub fn words(&self) -> String {
        let mut parts = vec![if self.version.is_empty() { "ComfyUI".to_string() } else { format!("ComfyUI {}", self.version) }];
        if !self.devices.is_empty() {
            parts.push(self.devices.join(", "));
        }
        parts.push(match self.checkpoints.len() {
            1 => "1 checkpoint".to_string(),
            n => format!("{n} checkpoints"),
        });
        parts.join(" · ")
    }
}

/// Ask a server what it is and which checkpoints it has.
pub fn check(address: &str) -> Result<ComfyStatus, ToolError> {
    let base = clean_address(address)?;
    let stats = get_json(&base, "/system_stats")?;
    let version = stats.pointer("/system/comfyui_version").and_then(Value::as_str).unwrap_or_default().to_string();
    let devices_raw = stats.get("devices").and_then(Value::as_array).cloned().unwrap_or_default();
    let devices: Vec<String> = devices_raw.iter().filter_map(|d| d.get("name").and_then(Value::as_str)).map(device_name).collect();
    let vram_gb = devices_raw.first().and_then(|d| d.get("vram_total")).and_then(Value::as_u64).filter(|b| *b > 0).map(|b| b >> 30);
    let info = get_json(&base, "/object_info/CheckpointLoaderSimple")?;
    let checkpoints = checkpoint_names(&info);
    Ok(ComfyStatus { version, devices, vram_gb, checkpoints })
}

/// The checkpoint names in `/object_info/CheckpointLoaderSimple`, in either shape: the old
/// `ckpt_name: [[names], {..}]` or the new `["COMBO", {"options": [names]}]`.
fn checkpoint_names(info: &Value) -> Vec<String> {
    let spec = info.pointer("/CheckpointLoaderSimple/input/required/ckpt_name").and_then(Value::as_array);
    let names = match spec.and_then(|s| s.first()) {
        Some(Value::Array(names)) => Some(names),
        Some(Value::String(kind)) if kind == "COMBO" => spec.and_then(|s| s.get(1)).and_then(|o| o.get("options")).and_then(Value::as_array),
        _ => None,
    };
    names.map(|n| n.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// "cuda:0 NVIDIA GeForce RTX 3060 : cudaMallocAsync" becomes "NVIDIA GeForce RTX 3060".
fn device_name(raw: &str) -> String {
    let name = raw.split(" : ").next().unwrap_or(raw).trim();
    let name = match name.split_once(' ') {
        Some((head, rest)) if head.contains(':') && head.chars().all(|c| c.is_ascii_alphanumeric() || c == ':') => rest,
        _ => name,
    };
    name.trim().to_string()
}

fn get_json(base: &str, path: &str) -> Result<Value, ToolError> {
    let (status, text) = send(agent(Duration::from_secs(20)).get(&format!("{base}{path}")).call(), base)?;
    if !(200..300).contains(&status) {
        return Err(http_error(base, status, &text));
    }
    serde_json::from_str(&text).map_err(|_| garbled("sent an unreadable answer"))
}

/// The address with its trailing slashes gone, if it's a web address with a host.
pub fn clean_address(address: &str) -> Result<String, ToolError> {
    let a = address.trim().trim_end_matches('/');
    let host = a.strip_prefix("http://").or_else(|| a.strip_prefix("https://")).filter(|rest| !rest.is_empty() && !rest.starts_with('/'));
    match host {
        Some(_) if !a.chars().any(char::is_whitespace) => Ok(a.to_string()),
        _ => Err(ToolError::new(ErrorCode::BadSettings, "The ComfyUI address isn't a web address.")
            .with_remediation("Give it as http://host:port, for example http://192.168.1.20:8188.")),
    }
}

fn size(shape: Shape) -> (u32, u32) {
    match shape {
        Shape::Square => (1024, 1024),
        Shape::Wide => (1024, 576),
        Shape::Tall => (576, 1024),
    }
}

/// From the clock, so no two pictures share one (kept within what JSON numbers hold exactly).
fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) % (1 << 53)
}

struct Vars<'a> {
    prompt: &'a str,
    negative: &'a str,
    width: u32,
    height: u32,
    seed: u64,
    checkpoint: &'a str,
}

fn builtin_graph(v: &Vars) -> Value {
    json!({
        "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": v.checkpoint}},
        "2": {"class_type": "CLIPTextEncode", "inputs": {"text": v.prompt, "clip": ["1", 1]}},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"text": v.negative, "clip": ["1", 1]}},
        "4": {"class_type": "EmptyLatentImage", "inputs": {"width": v.width, "height": v.height, "batch_size": 1}},
        "5": {"class_type": "KSampler", "inputs": {
            "seed": v.seed, "steps": 25, "cfg": 6.0, "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0,
            "model": ["1", 0], "positive": ["2", 0], "negative": ["3", 0], "latent_image": ["4", 0]}},
        "6": {"class_type": "VAEDecode", "inputs": {"samples": ["5", 0], "vae": ["1", 2]}},
        "7": {"class_type": "SaveImage", "inputs": {"filename_prefix": "craft-anything", "images": ["6", 0]}},
    })
}

/// The person's API-format workflow, with the placeholders in its string values filled in.
fn from_workflow(path: &std::path::Path, vars: &Vars) -> Result<Value, ToolError> {
    let bad = |why: String| {
        ToolError::new(ErrorCode::BadSettings, format!("The ComfyUI workflow file can't be used: {why}"))
            .with_remediation("In ComfyUI choose Export (API), save the file, and pick that file in Settings → Builds → Picture model.")
    };
    let file = std::fs::File::open(path).map_err(|e| bad(format!("{} ({e})", path.display())))?;
    let bytes = read_capped(file, MAX_WORKFLOW).map_err(|e| match e.code {
        ErrorCode::OverLimit => bad("it is over 2 MB".into()),
        _ => bad(format!("{} ({})", path.display(), e.message)),
    })?;
    let mut graph: Value = serde_json::from_slice(&bytes).map_err(|e| bad(format!("it isn't JSON ({e})")))?;
    // Every node names its class: anything else (a settings or credentials file picked by mistake)
    // is never sent to the server.
    let api_graph = graph.as_object().is_some_and(|nodes| !nodes.is_empty() && nodes.values().all(|n| n.get("class_type").is_some_and(Value::is_string)));
    if !api_graph {
        return Err(bad("it isn't an API-format graph (an object of nodes)".into()));
    }
    fill(&mut graph, vars);
    Ok(graph)
}

/// Fill the placeholders in every string value. A string that is just `{{width}}`, `{{height}}` or
/// `{{seed}}` becomes a number.
fn fill(v: &mut Value, vars: &Vars) {
    match v {
        Value::String(s) => {
            let whole = match s.as_str() {
                "{{width}}" => Some(Value::from(vars.width)),
                "{{height}}" => Some(Value::from(vars.height)),
                "{{seed}}" => Some(Value::from(vars.seed)),
                _ => None,
            };
            match whole {
                Some(n) => *v = n,
                None => *s = substitute(s, vars),
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| fill(i, vars)),
        Value::Object(map) => map.values_mut().for_each(|i| fill(i, vars)),
        _ => {}
    }
}

/// One left-to-right pass, so text put in by a placeholder is never read as another placeholder.
fn substitute(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(rest.get(..start).unwrap_or_default());
        let after = rest.get(start..).unwrap_or_default();
        let Some(end) = after.find("}}") else {
            out.push_str(after);
            return out;
        };
        let name = after.get(2..end).unwrap_or_default();
        let value = match name {
            "prompt" => Some(vars.prompt.to_string()),
            "negative" => Some(vars.negative.to_string()),
            "width" => Some(vars.width.to_string()),
            "height" => Some(vars.height.to_string()),
            "seed" => Some(vars.seed.to_string()),
            "checkpoint" => Some(vars.checkpoint.to_string()),
            _ => None,
        };
        match value {
            Some(v) => out.push_str(&v),
            None => out.push_str(after.get(..end + 2).unwrap_or_default()),
        }
        rest = after.get(end + 2..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

struct Image {
    filename: String,
    subfolder: String,
    kind: String,
}

/// The first picture a finished job saved (one it kept, before one it only previewed).
fn first_image(entry: &Value) -> Option<Image> {
    let outputs = entry.get("outputs").and_then(Value::as_object)?;
    let all: Vec<&Value> = outputs.values().filter_map(|o| o.get("images").and_then(Value::as_array)).flatten().collect();
    let pick = all.iter().find(|i| i.get("type").and_then(Value::as_str) == Some("output")).or_else(|| all.first())?;
    let text = |k: &str| pick.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let filename = text("filename");
    if filename.is_empty() {
        return None;
    }
    let kind = Some(text("type")).filter(|k| !k.is_empty()).unwrap_or_else(|| "output".into());
    Some(Image { filename, subfolder: text("subfolder"), kind })
}

/// A 400 from `/prompt`: which node ComfyUI refused, and why.
fn refused(v: &Value, checkpoint: &str) -> ToolError {
    let mut why = String::new();
    if let Some(nodes) = v.get("node_errors").and_then(Value::as_object) {
        for (id, node) in nodes {
            let class = node.get("class_type").and_then(Value::as_str).unwrap_or("a node");
            let msgs: Vec<String> = node
                .get("errors")
                .and_then(Value::as_array)
                .map(|e| {
                    e.iter()
                        .map(|e| {
                            let m = e.get("message").and_then(Value::as_str).unwrap_or_default();
                            match e.get("details").and_then(Value::as_str).filter(|d| !d.is_empty()) {
                                Some(d) => format!("{m} ({d})"),
                                None => m.to_string(),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !why.is_empty() {
                why.push_str("; ");
            }
            why.push_str(&format!("{class} (node {id}): {}", msgs.join(", ")));
        }
    }
    if why.is_empty() {
        why = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("it didn't say why").to_string();
    }
    let why: String = why.chars().take(400).collect();
    ToolError::new(ErrorCode::ProgramFailed, format!("ComfyUI refused the picture: {why}")).with_remediation(format!(
        "Check that the checkpoint \"{checkpoint}\" is installed on the server (Settings → Builds → Picture model, then Test), and that a workflow file, if you use one, was exported in API format."
    ))
}

/// A job that ran and failed: the node and message ComfyUI recorded.
fn failed(entry: &Value) -> ToolError {
    let detail = entry
        .pointer("/status/messages")
        .and_then(Value::as_array)
        .and_then(|m| m.iter().find_map(|m| m.as_array().filter(|a| a.first().and_then(Value::as_str) == Some("execution_error")).and_then(|a| a.get(1))))
        .map(|e| {
            let node = e.get("node_type").and_then(Value::as_str).unwrap_or("a node");
            let msg = e.get("exception_message").and_then(Value::as_str).unwrap_or("it didn't say why");
            format!("{node}: {}", msg.trim())
        })
        .unwrap_or_else(|| "it didn't say why".to_string());
    let detail: String = detail.chars().take(400).collect();
    ToolError::new(ErrorCode::ProgramFailed, format!("ComfyUI couldn't make the picture: {detail}"))
        .with_remediation("Look at the ComfyUI window for the details, then try again (a model that needs more memory than the server has is a common cause).")
}

fn garbled(what: &str) -> ToolError {
    ToolError::new(ErrorCode::GarbledOutput, format!("ComfyUI {what}."))
        .with_remediation("Check that the address is a ComfyUI server and that it is up to date.")
}

fn unreachable_server(base: &str, why: &str) -> ToolError {
    ToolError::new(ErrorCode::EngineMissing, format!("Couldn't reach ComfyUI at {base}: {why}"))
        .with_remediation("Start ComfyUI and check the address (it listens on port 8188 unless told otherwise).")
}

/// Only ComfyUI's own `error.message` is passed on, never the raw answer: the address can come
/// from a builder agent, so another program's page must not reach it.
fn http_error(base: &str, status: u16, text: &str) -> ToolError {
    let why = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(|m| format!(": {}", m.chars().take(200).collect::<String>())))
        .unwrap_or_default();
    ToolError::new(ErrorCode::EngineMissing, format!("ComfyUI at {base} answered {status}{why}"))
        .with_remediation("Check that the address is a ComfyUI server (not another program on that port).")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // Never follow a redirect: every request stays on the address the person gave.
        .max_redirects(0)
        .user_agent("CraftAnything")
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

/// A finished request's status and capped text, or the plain reason it couldn't be made.
fn send(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>, base: &str) -> Result<(u16, String), ToolError> {
    let resp = result.map_err(|e| unreachable_server(base, &e.to_string()))?;
    let status = resp.status().as_u16();
    let bytes = read_capped(resp.into_body().into_reader(), MAX_JSON)?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Everything up to `cap` bytes; more is an error, never an unbounded read.
fn read_capped(reader: impl Read, cap: u64) -> Result<Vec<u8>, ToolError> {
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes).map_err(|e| ToolError::new(ErrorCode::EngineMissing, format!("The answer broke off: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(ToolError::new(ErrorCode::OverLimit, "ComfyUI sent more than the size limit."));
    }
    Ok(bytes)
}

/// Percent-encode a query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Catalogue;
    use craft_anything_platform::sync::Mutex;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    type Seen = Arc<Mutex<Vec<(String, String)>>>;

    /// A stand-in ComfyUI: `route(method_and_path, body)` answers `(status, content type, bytes)`.
    /// Returns its address and every (request line, body) it was sent.
    fn serve(route: impl Fn(&str, &str) -> (u16, &'static str, Vec<u8>) + Send + 'static) -> (String, Seen) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for stream in l.incoming().flatten() {
                let mut stream = stream;
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if r.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let body = String::from_utf8_lossy(&body).into_owned();
                let line = request_line.trim().trim_end_matches(" HTTP/1.1").to_string();
                let (status, kind, out) = route(&line, &body);
                s2.lock().unwrap().push((line, body));
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = stream.write_all(&out);
            }
        });
        (base, seen)
    }

    fn json_out(v: Value) -> (u16, &'static str, Vec<u8>) {
        (200, "application/json", v.to_string().into_bytes())
    }

    /// A server that accepts a job, finishes it at once and serves `png`.
    fn happy(png: &'static [u8]) -> (String, Seen) {
        serve(move |line, _| {
            if line.starts_with("POST /prompt") {
                json_out(json!({"prompt_id": "abc-1", "number": 1, "node_errors": {}}))
            } else if line.starts_with("GET /history/abc-1") {
                json_out(json!({"abc-1": {"status": {"status_str": "success", "completed": true, "messages": []},
                    "outputs": {"7": {"images": [{"filename": "craft anything_00001_.png", "subfolder": "sub dir", "type": "output"}]}}}}))
            } else if line.starts_with("GET /view") {
                (200, "image/png", png.to_vec())
            } else {
                (404, "text/plain", b"no".to_vec())
            }
        })
    }

    fn picture(c: &ComfyUi, prompt: &str, shape: Shape) -> Result<Generated, ToolError> {
        let model = Catalogue::built_in().image_model(Some("comfyui")).unwrap();
        c.generate("", &AssetRequest::Image { model, prompt, shape })
    }

    #[test]
    fn a_picture_is_made_end_to_end_and_costs_nothing() {
        let (base, seen) = happy(b"\x89PNG fake");
        let c = ComfyUi::new(format!("{base}/"), "sdxl base.safetensors", None);
        let g = picture(&c, "a wooden crate", Shape::Wide).unwrap();
        assert_eq!((g.bytes.as_slice(), g.cost_usd), (&b"\x89PNG fake"[..], 0.0));
        assert!(g.url.starts_with("data:image/png;base64,"), "a local server's address means nothing to another service");
        let seen = seen.lock().unwrap();
        let posted: Value = serde_json::from_str(&seen[0].1).unwrap();
        assert_eq!(posted["client_id"], "craft-anything");
        let g = &posted["prompt"];
        assert_eq!(g["1"]["inputs"]["ckpt_name"], "sdxl base.safetensors");
        assert_eq!(g["2"]["inputs"]["text"], "a wooden crate");
        assert_eq!(g["3"]["inputs"]["text"], NEGATIVE);
        assert_eq!((&g["4"]["inputs"]["width"], &g["4"]["inputs"]["height"]), (&json!(1024), &json!(576)));
        assert_eq!((&g["5"]["inputs"]["steps"], &g["5"]["inputs"]["sampler_name"]), (&json!(25), &json!("euler")));
        assert!(g["5"]["inputs"]["seed"].is_u64());
        assert_eq!(g["7"]["inputs"]["filename_prefix"], "craft-anything");
        // The name and folder are percent-encoded in the fetch.
        assert!(seen.iter().any(|(l, _)| l == "GET /view?filename=craft%20anything_00001_.png&subfolder=sub%20dir&type=output"), "{seen:?}");
    }

    #[test]
    fn the_shapes_have_their_sizes() {
        assert_eq!((size(Shape::Square), size(Shape::Wide), size(Shape::Tall)), ((1024, 1024), (1024, 576), (576, 1024)));
    }

    #[test]
    fn a_workflow_file_gets_its_placeholders_filled_and_numbers_made_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("flow.json");
        std::fs::write(
            &file,
            json!({"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "{{checkpoint}}"}},
                "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "photo of {{prompt}}, {{width}}x{{height}}"}},
                "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{negative}}"}},
                "4": {"class_type": "EmptyLatentImage", "inputs": {"width": "{{width}}", "height": "{{height}}", "batch_size": 1}},
                "5": {"class_type": "KSampler", "inputs": {"seed": "{{seed}}", "extra": ["{{prompt}}", "{{unknown}}"]}}})
            .to_string(),
        )
        .unwrap();
        let (base, seen) = happy(b"png");
        let c = ComfyUi::new(base, "m.ckpt", Some(file));
        // A prompt that looks like a placeholder is put in as text, not read again.
        picture(&c, "a {{seed}} crate", Shape::Tall).unwrap();
        let posted: Value = serde_json::from_str(&seen.lock().unwrap()[0].1).unwrap();
        let g = &posted["prompt"];
        assert_eq!(g["1"]["inputs"]["ckpt_name"], "m.ckpt");
        assert_eq!(g["2"]["inputs"]["text"], "photo of a {{seed}} crate, 576x1024");
        assert_eq!(g["3"]["inputs"]["text"], NEGATIVE);
        assert_eq!((&g["4"]["inputs"]["width"], &g["4"]["inputs"]["height"]), (&json!(576), &json!(1024)), "numbers, not strings");
        assert!(g["5"]["inputs"]["seed"].is_u64());
        assert_eq!(g["5"]["inputs"]["extra"], json!(["a {{seed}} crate", "{{unknown}}"]));
    }

    #[test]
    fn a_workflow_that_isnt_usable_says_how_to_export_one() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = happy(b"png");
        let missing = ComfyUi::new(base.clone(), "m", Some(dir.path().join("none.json")));
        assert_eq!(picture(&missing, "x", Shape::Square).unwrap_err().code, ErrorCode::BadSettings);
        let file = dir.path().join("list.json");
        std::fs::write(&file, "[1, 2]").unwrap();
        let err = picture(&ComfyUi::new(base.clone(), "m", Some(file)), "x", Shape::Square).unwrap_err();
        assert!(err.remediation.unwrap_or_default().contains("Export (API)"));
        let big = dir.path().join("big.json");
        std::fs::write(&big, vec![b' '; (MAX_WORKFLOW + 10) as usize]).unwrap();
        assert!(picture(&ComfyUi::new(base, "m", Some(big)), "x", Shape::Square).unwrap_err().message.contains("2 MB"));
    }

    /// Only a graph of nodes is sent: a settings or credentials file picked by mistake, or an empty
    /// object, is refused before anything reaches the server.
    #[test]
    fn a_json_file_that_isnt_a_graph_of_nodes_is_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let (base, seen) = happy(b"png");
        for (name, body) in
            [("keys.json", r#"{"api_key": "sk-test"}"#), ("empty.json", "{}"), ("half.json", r#"{"1": {"class_type": "SaveImage"}, "2": {"inputs": {}}}"#)]
        {
            let file = dir.path().join(name);
            std::fs::write(&file, body).unwrap();
            let err = picture(&ComfyUi::new(base.clone(), "m", Some(file)), "x", Shape::Square).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadSettings, "{name}");
        }
        assert!(seen.lock().unwrap().iter().all(|(line, _)| !line.starts_with("POST /prompt")), "nothing was posted");
    }

    /// A file that can't be read says why, not that it is too big.
    /// A check never follows a redirect elsewhere, and another program's page never reaches the error.
    #[test]
    fn a_check_stays_on_its_address_and_never_repeats_another_programs_page() {
        let (elsewhere, hit) = serve(|_, _| json_out(json!({"system": {"comfyui_version": "0.3.10"}})));
        let to: &'static str = Box::leak(format!("text/plain\r\nLocation: {elsewhere}/system_stats").into_boxed_str());
        let (base, _) = serve(move |_, _| (302, to, Vec::new()));
        let err = check(&base).unwrap_err();
        assert!(err.message.contains("answered 302"), "{}", err.message);
        assert!(hit.lock().unwrap().is_empty(), "the redirect was followed");
        let (base, _) = serve(|_, _| (500, "text/html", b"<html>internal admin page</html>".to_vec()));
        let err = check(&base).unwrap_err();
        assert!(err.message.ends_with("answered 500") && !err.message.contains("admin"), "{}", err.message);
        let (base, _) = serve(|_, _| (500, "application/json", json!({"error": {"message": "out of memory"}}).to_string().into_bytes()));
        assert!(check(&base).unwrap_err().message.ends_with("answered 500: out of memory"));
    }

    #[cfg(unix)]
    #[test]
    fn a_workflow_that_cant_be_read_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = happy(b"png");
        // A folder opens on Unix and fails on the read.
        let err = picture(&ComfyUi::new(base, "m", Some(dir.path().to_path_buf())), "x", Shape::Square).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadSettings);
        assert!(!err.message.contains("2 MB"), "{err:?}");
    }

    #[test]
    fn both_object_info_shapes_list_their_checkpoints() {
        let old = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["a.safetensors", "b.ckpt"], {}]}}}});
        let new = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": ["COMBO", {"options": ["a.safetensors", "b.ckpt"]}]}}}});
        assert_eq!(checkpoint_names(&old), vec!["a.safetensors", "b.ckpt"]);
        assert_eq!(checkpoint_names(&new), vec!["a.safetensors", "b.ckpt"]);
        assert!(checkpoint_names(&json!({})).is_empty());
        assert!(checkpoint_names(&json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": 3}}}})).is_empty());
    }

    #[test]
    fn check_reads_the_version_the_card_and_the_checkpoints() {
        let (base, _) = serve(|line, _| {
            if line.starts_with("GET /system_stats") {
                json_out(json!({"system": {"comfyui_version": "0.3.10", "os": "posix"},
                    "devices": [{"name": "cuda:0 NVIDIA GeForce RTX 3060 : cudaMallocAsync", "type": "cuda", "vram_total": 12_884_901_888u64}]}))
            } else if line.starts_with("GET /object_info/CheckpointLoaderSimple") {
                json_out(json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": ["COMBO", {"options": ["a.safetensors", "b.ckpt"]}]}}}}))
            } else {
                (404, "text/plain", vec![])
            }
        });
        let s = check(&format!("{base}/")).unwrap();
        assert_eq!(s.version, "0.3.10");
        assert_eq!(s.devices, vec!["NVIDIA GeForce RTX 3060"]);
        assert_eq!((s.vram_gb, s.checkpoints.len()), (Some(12), 2));
        assert_eq!(s.words(), "ComfyUI 0.3.10 · NVIDIA GeForce RTX 3060 · 2 checkpoints");
    }

    #[test]
    fn a_node_error_becomes_a_plain_error_naming_the_node() {
        let (base, _) = serve(|_, _| {
            (
                400,
                "application/json",
                json!({"error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation"},
                    "node_errors": {"1": {"class_type": "CheckpointLoaderSimple", "errors": [{"type": "value_not_in_list",
                        "message": "Value not in list", "details": "ckpt_name: 'nope.ckpt' not in ['a.ckpt']"}]}}})
                .to_string()
                .into_bytes(),
            )
        });
        let err = picture(&ComfyUi::new(base, "nope.ckpt", None), "x", Shape::Square).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProgramFailed);
        assert!(err.message.contains("CheckpointLoaderSimple (node 1)") && err.message.contains("Value not in list"), "{}", err.message);
        assert!(err.remediation.unwrap_or_default().contains("nope.ckpt"));
        // A 400 with only an error message.
        let (base, _) = serve(|_, _| (400, "application/json", json!({"error": {"message": "bad graph"}}).to_string().into_bytes()));
        assert!(picture(&ComfyUi::new(base, "m", None), "x", Shape::Square).unwrap_err().message.contains("bad graph"));
    }

    #[test]
    fn a_job_that_ends_in_error_is_a_plain_error() {
        let (base, _) = serve(|line, _| {
            if line.starts_with("POST /prompt") {
                json_out(json!({"prompt_id": "j1"}))
            } else {
                json_out(json!({"j1": {"status": {"status_str": "error", "completed": false, "messages": [
                    ["execution_start", {}], ["execution_error", {"node_type": "KSampler", "exception_message": "CUDA out of memory\n"}]]}, "outputs": {}}}))
            }
        });
        let err = picture(&ComfyUi::new(base, "m", None), "x", Shape::Square).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProgramFailed);
        assert!(err.message.contains("KSampler: CUDA out of memory"), "{}", err.message);
    }

    #[test]
    fn a_job_that_never_finishes_times_out_and_a_closed_port_is_plain() {
        let (base, _) = serve(|line, _| if line.starts_with("POST /prompt") { json_out(json!({"prompt_id": "j"})) } else { json_out(json!({})) });
        let mut c = ComfyUi::new(base, "m", None);
        c.poll = Duration::from_millis(10);
        c.deadline = Duration::from_millis(80);
        assert_eq!(picture(&c, "x", Shape::Square).unwrap_err().code, ErrorCode::TimedOut);
        let err = picture(&ComfyUi::new("http://127.0.0.1:9", "m", None), "x", Shape::Square).unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing);
        assert!(err.remediation.unwrap_or_default().contains("8188"));
    }

    #[test]
    fn addresses_need_a_scheme_and_a_host_and_a_checkpoint_is_needed() {
        assert_eq!(clean_address(" http://192.168.1.20:8188// ").unwrap(), "http://192.168.1.20:8188");
        assert_eq!(clean_address("https://comfy.example").unwrap(), "https://comfy.example");
        for bad in ["", "192.168.1.20:8188", "http://", "ftp://x", "http:///x", "http://a b"] {
            assert_eq!(clean_address(bad).unwrap_err().code, ErrorCode::BadSettings, "{bad:?}");
        }
        assert_eq!(picture(&ComfyUi::new("http://127.0.0.1:9", "  ", None), "x", Shape::Square).unwrap_err().code, ErrorCode::BadSettings);
        assert!(!ComfyUi::new("", "", None).needs_key());
        assert_eq!(device_name("cpu"), "cpu");
        assert_eq!(device_name("cuda:0 NVIDIA GeForce RTX 3060 : cudaMallocAsync"), "NVIDIA GeForce RTX 3060");
    }

    /// Against a real server: `COMFYUI_URL` and `COMFYUI_CHECKPOINT`. Run with `--ignored`.
    #[test]
    #[ignore = "needs a ComfyUI server (COMFYUI_URL, COMFYUI_CHECKPOINT)"]
    fn live_one_square_picture() {
        let (Ok(url), Ok(checkpoint)) = (std::env::var("COMFYUI_URL"), std::env::var("COMFYUI_CHECKPOINT")) else {
            eprintln!("set COMFYUI_URL and COMFYUI_CHECKPOINT");
            return;
        };
        let status = check(&url).unwrap();
        eprintln!("{}", status.words());
        let g = picture(&ComfyUi::new(url, checkpoint, None), "a small wooden crate, game asset, plain background", Shape::Square).unwrap();
        let path = std::env::temp_dir().join("craft-anything-comfyui-live.png");
        std::fs::write(&path, &g.bytes).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
