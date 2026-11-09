//! Anthropic Messages API: streaming SSE, tools, adaptive thinking, effort, prompt caching and
//! server-side fallbacks.

use crate::http::{self, SseEvent};
use crate::model::{Block, ChatModel, Delta, Effort, Msg, Speaker, Stop, StopReason, ToolDef, TurnRequest, TurnResult, Usage};
use craft_anything_evidence::{ErrorCode, ToolError};
use serde_json::{Value, json};

pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// Whether an address is Anthropic's own API, which speaks this protocol and not OpenAI's (#1100).
pub fn is_anthropic_api(address: &str) -> bool {
    crate::openai::https_host_path(address).is_some_and(|(host, _)| host.eq_ignore_ascii_case("api.anthropic.com"))
}
/// Offered in the model dropdown when the live list can't be fetched: the 5.x models first, then the
/// older 4.x ones Claude Code still offers (#1289). Ids as Claude Code 2.1.319 names them.
pub const MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-opus-4-5",
    "claude-sonnet-4-5",
];
/// How a model takes extended thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    /// `thinking: {type: "adaptive", display: "summarized"}`, the only mode of Claude 4.6 and later.
    Adaptive,
    /// No `thinking` field. Models before 4.6 (Opus 4.5, Sonnet 4.5, Haiku 4.5) take only a manual
    /// `budget_tokens`, which we don't set; they run without thinking, as they do by default.
    Off,
}

/// What one model accepts in a Messages request. Checked against Anthropic's model reference
/// (the `claude-api` skill, 2026-11): a field a model doesn't take is a 400, so each model gets
/// only what it takes.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// The model id; a date suffix (`-20251101`) also matches.
    pub id: &'static str,
    pub thinking: Thinking,
    /// The effort levels it takes, lowest first; `None`: no `output_config.effort` at all.
    pub effort: Option<&'static [Effort]>,
    /// The server-side `fallbacks: "default"` field (beta) is accepted. Haiku 5.5 has no
    /// server-side fallback, and the other models named here aren't listed as taking it.
    pub fallbacks: bool,
    /// `eager_input_streaming` on client tools is accepted. Not confirmed for models before 4.6,
    /// so those get the buffered default, which is always accepted.
    pub eager_input: bool,
}

const ALL_LEVELS: &[Effort] = &Effort::ALL;
/// Claude 4.6 models: no `xhigh`.
const NO_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];
/// Opus 4.5: `low`, `medium` and `high` only.
const UP_TO_HIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

const fn adaptive(id: &'static str, effort: &'static [Effort], fallbacks: bool) -> Profile {
    Profile { id, thinking: Thinking::Adaptive, effort: Some(effort), fallbacks, eager_input: true }
}

/// Every model we name. A model not listed gets [`UNKNOWN`].
pub const PROFILES: &[Profile] = &[
    adaptive("claude-opus-5-5", ALL_LEVELS, true),
    adaptive("claude-opus-5", ALL_LEVELS, true),
    adaptive("claude-sonnet-5-5", ALL_LEVELS, true),
    adaptive("claude-sonnet-5", ALL_LEVELS, false),
    adaptive("claude-haiku-5-5", ALL_LEVELS, false),
    adaptive("claude-fable-5-1", ALL_LEVELS, true),
    adaptive("claude-fable-5", ALL_LEVELS, false),
    adaptive("claude-opus-4-8", ALL_LEVELS, false),
    adaptive("claude-opus-4-7", ALL_LEVELS, false),
    adaptive("claude-opus-4-6", NO_XHIGH, false),
    adaptive("claude-sonnet-4-6", NO_XHIGH, false),
    Profile { id: "claude-opus-4-5", thinking: Thinking::Off, effort: Some(UP_TO_HIGH), fallbacks: false, eager_input: false },
    Profile { id: "claude-sonnet-4-5", thinking: Thinking::Off, effort: None, fallbacks: false, eager_input: false },
    Profile { id: "claude-haiku-4-5", thinking: Thinking::Off, effort: None, fallbacks: false, eager_input: false },
];

/// A model we don't know yet: adaptive thinking and every effort level (the current generation),
/// without the fallback field, which only the models above are known to take.
pub const UNKNOWN: Profile = adaptive("", ALL_LEVELS, false);

/// The profile for `model`: the longest id it matches (exactly, or with a date suffix), else
/// [`UNKNOWN`]. Provider prefixes (`anthropic/`, `anthropic.`) are ignored.
pub fn profile_for(model: &str) -> Profile {
    let model = model.trim().trim_start_matches("anthropic/").trim_start_matches("anthropic.");
    let matches = |p: &&Profile| model == p.id || model.strip_prefix(p.id).is_some_and(|rest| rest.starts_with('-'));
    PROFILES.iter().filter(matches).max_by_key(|p| p.id.len()).copied().unwrap_or(UNKNOWN)
}

/// The effort level `model` is sent: `effort`, or the nearest level below it the model has.
/// `None` when the model takes no effort parameter at all.
pub fn effort_for(model: &str, effort: Effort) -> Option<Effort> {
    let levels = profile_for(model).effort?;
    Some(effort.within(&Effort::ALL).within(levels))
}
pub const API_VERSION: &str = "2023-06-01";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const MAX_TOKENS: u32 = 64_000;

/// Most content blocks we track in one response.
const MAX_BLOCKS: usize = 4096;

#[derive(Debug, Clone)]
pub struct Anthropic {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Send the server-side fallback header and body field.
    pub fallbacks: bool,
    /// The AI's own limit on one streamed answer; none uses the defaults.
    pub stream_limit: Option<std::time::Duration>,
}

impl Anthropic {
    pub fn new(base_url: Option<&str>, api_key: String, model: &str) -> Self {
        Anthropic {
            base_url: base_url.unwrap_or(DEFAULT_BASE).trim_end_matches('/').to_string(),
            api_key,
            model: if model.is_empty() { DEFAULT_MODEL.to_string() } else { model.to_string() },
            fallbacks: true,
            stream_limit: None,
        }
    }

    fn url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches("/v1");
        format!("{base}/v1/{path}")
    }

    /// Whether this request asks for server-side fallbacks: the AI allows them and the model takes them.
    pub fn sends_fallbacks(&self) -> bool {
        self.fallbacks && profile_for(&self.model).fallbacks
    }

    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("x-api-key", self.api_key.clone()), ("anthropic-version", API_VERSION.to_string())];
        if self.sends_fallbacks() {
            h.push(("anthropic-beta", FALLBACK_BETA.to_string()));
        }
        h
    }

    /// The request body for one streamed turn. Only the fields this model takes (see [`Profile`]).
    pub fn body(&self, req: &TurnRequest<'_>) -> Value {
        let profile = profile_for(&self.model);
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
            "messages": req.messages.iter().map(message_json).collect::<Vec<_>>(),
        });
        if let Some(m) = body.as_object_mut() {
            if profile.thinking == Thinking::Adaptive {
                m.insert("thinking".into(), json!({"type": "adaptive", "display": "summarized"}));
            }
            if let Some(effort) = effort_for(&self.model, req.effort) {
                m.insert("output_config".into(), json!({"effort": effort.as_str()}));
            }
        }
        if !req.tools.is_empty() {
            let n = req.tools.len();
            let tools: Vec<Value> = req
                .tools
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let mut v = json!({"name": t.name, "description": t.description, "input_schema": t.schema});
                    if profile.eager_input
                        && let Some(m) = v.as_object_mut()
                    {
                        m.insert("eager_input_streaming".into(), json!(true));
                    }
                    if i + 1 == n
                        && let Some(m) = v.as_object_mut()
                    {
                        m.insert("cache_control".into(), json!({"type": "ephemeral"}));
                    }
                    v
                })
                .collect();
            if let Some(m) = body.as_object_mut() {
                m.insert("tools".into(), Value::Array(tools));
                m.insert("tool_choice".into(), json!({"type": "auto"}));
            }
        }
        if self.sends_fallbacks()
            && let Some(m) = body.as_object_mut()
        {
            m.insert("fallbacks".into(), json!("default"));
        }
        body
    }

    /// Model ids available to this key.
    pub fn list_models(&self, timeout: std::time::Duration) -> Result<Vec<String>, ToolError> {
        let agent = http::quick_agent(timeout);
        let v = http::get_json(&agent, "Anthropic", &self.url("models"), &self.headers())?;
        Ok(v.get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string)).collect())
            .unwrap_or_default())
    }
}

pub fn message_json(m: &Msg) -> Value {
    let role = match m.role {
        Speaker::User => "user",
        Speaker::Assistant => "assistant",
    };
    let content: Vec<Value> = m
        .blocks
        .iter()
        .filter_map(|b| {
            Some(match b {
                Block::Text { text } => json!({"type": "text", "text": text}),
                Block::Thinking { thinking, signature } => json!({"type": "thinking", "thinking": thinking, "signature": signature}),
                Block::RedactedThinking { data } => json!({"type": "redacted_thinking", "data": data}),
                Block::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
                Block::ToolExtra { .. } => return None,
                Block::ToolResult { tool_use_id, content, is_error } => {
                    json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error})
                }
            })
        })
        .collect();
    json!({"role": role, "content": content})
}

#[derive(Debug, Default)]
enum Partial {
    #[default]
    Empty,
    Text(String),
    Thinking(String, String),
    Redacted(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

/// Accumulates one streamed response.
#[derive(Debug, Default)]
pub struct StreamState {
    blocks: Vec<Partial>,
    pub usage: Usage,
    pub stop: Option<StopReason>,
    pub model: String,
    pub done: bool,
}

impl StreamState {
    fn slot(&mut self, index: usize) -> Option<&mut Partial> {
        if index >= MAX_BLOCKS {
            return None;
        }
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, Partial::default);
        }
        self.blocks.get_mut(index)
    }

    /// Apply one SSE event.
    pub fn apply(&mut self, ev: &SseEvent, on_delta: &mut dyn FnMut(Delta)) -> Result<(), ToolError> {
        if ev.data.is_empty() {
            return Ok(());
        }
        let v: Value =
            serde_json::from_str(&ev.data).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("Anthropic sent an event we couldn't read: {e}")))?;
        let kind = v.get("type").and_then(Value::as_str).or(ev.event.as_deref()).unwrap_or_default();
        let index = v.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
        match kind {
            "message_start" => {
                let m = v.get("message").cloned().unwrap_or(Value::Null);
                self.model = m.get("model").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(u) = m.get("usage") {
                    self.read_usage(u);
                }
            }
            "content_block_start" => {
                let b = v.get("content_block").cloned().unwrap_or(Value::Null);
                let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let p = match b.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "text" => Partial::Text(s("text")),
                    "thinking" => Partial::Thinking(s("thinking"), s("signature")),
                    "redacted_thinking" => Partial::Redacted(s("data")),
                    "tool_use" | "server_tool_use" => Partial::Tool { id: s("id"), name: s("name"), json: String::new() },
                    _ => Partial::Empty,
                };
                if !s("text").is_empty() {
                    on_delta(Delta::Text(s("text")));
                }
                if let Some(slot) = self.slot(index) {
                    *slot = p;
                }
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let dtype = d.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
                let Some(slot) = self.slot(index) else { return Ok(()) };
                match (dtype.as_str(), slot) {
                    ("text_delta", Partial::Text(t)) => {
                        let piece = s("text");
                        t.push_str(&piece);
                        on_delta(Delta::Text(piece));
                    }
                    ("text_delta", slot @ Partial::Empty) => {
                        let piece = s("text");
                        *slot = Partial::Text(piece.clone());
                        on_delta(Delta::Text(piece));
                    }
                    ("thinking_delta", Partial::Thinking(t, _)) => {
                        let piece = s("thinking");
                        t.push_str(&piece);
                        on_delta(Delta::Thinking(piece));
                    }
                    ("signature_delta", Partial::Thinking(_, sig)) => sig.push_str(&s("signature")),
                    ("input_json_delta", Partial::Tool { json, .. }) => {
                        json.push_str(&s("partial_json"));
                        if json.len() > 4 * 1024 * 1024 {
                            return Err(ToolError::new(ErrorCode::OverLimit, "The model sent oversized tool input."));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" | "ping" => {}
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(StopReason::parse(r));
                }
                if let Some(u) = v.get("usage") {
                    self.read_usage(u);
                }
            }
            "message_stop" => self.done = true,
            "error" => {
                let et = v.pointer("/error/type").and_then(Value::as_str).unwrap_or("error");
                let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
                let code = match et {
                    "overloaded_error" | "api_error" => ErrorCode::EngineMissing,
                    "rate_limit_error" => ErrorCode::OverLimit,
                    "authentication_error" | "permission_error" => ErrorCode::NoPermission,
                    _ => ErrorCode::ProgramFailed,
                };
                return Err(ToolError::new(code, format!("Anthropic stream error ({et}): {msg}"))
                    .with_remediation("Try again; if it keeps happening, switch backend.")
                    .with_details(json!({"provider_error": et})));
            }
            _ => {}
        }
        Ok(())
    }

    fn read_usage(&mut self, u: &Value) {
        let g = |k: &str| u.get(k).and_then(Value::as_u64);
        if let Some(n) = g("input_tokens") {
            self.usage.input_tokens = n;
        }
        if let Some(n) = g("output_tokens") {
            self.usage.output_tokens = n;
        }
        if let Some(n) = g("cache_read_input_tokens") {
            self.usage.cache_read_tokens = n;
        }
        if let Some(n) = g("cache_creation_input_tokens") {
            self.usage.cache_write_tokens = n;
        }
    }

    /// The finished assistant message.
    pub fn finish(self, fallback_model: &str) -> Result<TurnResult, ToolError> {
        let mut blocks = Vec::new();
        for p in self.blocks {
            match p {
                Partial::Empty => {}
                Partial::Text(t) => blocks.push(Block::Text { text: t }),
                Partial::Thinking(t, s) => blocks.push(Block::Thinking { thinking: t, signature: s }),
                Partial::Redacted(d) => blocks.push(Block::RedactedThinking { data: d }),
                Partial::Tool { id, name, json } => {
                    // Eagerly streamed input is not validated by the server: a truncated or
                    // malformed object becomes a marked input the agent loop answers with an error.
                    let input = if json.trim().is_empty() { json!({}) } else { serde_json::from_str(&json).unwrap_or_else(|_| invalid_input(&json)) };
                    blocks.push(Block::ToolUse { id, name, input });
                }
            }
        }
        let stop = match self.stop {
            Some(s) => s,
            None if self.done => StopReason::EndTurn,
            None => return Err(ToolError::new(ErrorCode::Truncated, "The answer stream ended early.").with_remediation("Try again.")),
        };
        let model = if self.model.is_empty() { fallback_model.to_string() } else { self.model };
        Ok(TurnResult { message: Msg { role: Speaker::Assistant, blocks }, stop, usage: self.usage, model, follow_up: None })
    }
}

impl ChatModel for Anthropic {
    fn turn(&mut self, req: &TurnRequest<'_>, on_delta: &mut dyn FnMut(Delta), stop: &Stop) -> Result<TurnResult, ToolError> {
        let agent = http::stream_agent(self.stream_limit);
        let reader = http::post_json(&agent, "Anthropic", &self.url("messages"), &self.headers(), &self.body(req))?;
        let mut state = StreamState::default();
        http::parse_sse(reader, |ev| {
            stop.check()?;
            state.apply(&ev, on_delta)?;
            Ok(!state.done)
        })?;
        stop.check()?;
        state.finish(&self.model)
    }

    fn model(&self) -> String {
        self.model.clone()
    }
}

/// Marker for tool input that wasn't valid JSON.
pub const INVALID_INPUT_KEY: &str = "__invalid_json_input";

pub fn invalid_input(raw: &str) -> Value {
    json!({ INVALID_INPUT_KEY: raw.chars().take(200).collect::<String>() })
}

/// Tool definitions sorted by name so the cached prefix stays identical between requests.
pub fn sorted(mut tools: Vec<ToolDef>) -> Vec<ToolDef> {
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Effort;

    /// #1100: Anthropic's own address is told apart by its host, not by a look-alike.
    #[test]
    fn anthropics_own_address_is_recognised() {
        for yes in ["https://api.anthropic.com/v1", "https://api.anthropic.com", "HTTPS://API.Anthropic.com/v1/"] {
            assert!(is_anthropic_api(yes), "{yes}");
        }
        for no in ["http://api.anthropic.com/v1", "https://api.anthropic.com.example.test/v1", "https://proxy.test/api.anthropic.com/v1", "api.anthropic.com"] {
            assert!(!is_anthropic_api(no), "{no}");
        }
    }

    #[test]
    fn body_has_required_fields() {
        let a = Anthropic::new(None, "k".into(), "");
        let msgs = vec![Msg::user_text("hi")];
        let tools = vec![
            ToolDef { name: "a".into(), description: "d".into(), schema: json!({"type": "object"}) },
            ToolDef { name: "b".into(), description: "d".into(), schema: json!({"type": "object"}) },
        ];
        let b = a.body(&TurnRequest { system: "sys", messages: &msgs, tools: &tools, effort: Effort::High, max_tokens: MAX_TOKENS });
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["tool_choice"]["type"], "auto");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["max_tokens"], 64000);
        assert!(b["tools"][0].get("cache_control").is_none());
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(a.headers().iter().any(|(k, v)| *k == "anthropic-beta" && v == FALLBACK_BETA));
        let mut off = a.clone();
        off.fallbacks = false;
        assert!(off.body(&TurnRequest { system: "s", messages: &msgs, tools: &[], effort: Effort::Low, max_tokens: 10 }).get("fallbacks").is_none());
        assert!(off.headers().iter().all(|(k, _)| *k != "anthropic-beta"));
    }

    /// The dropdown offers the 5.x models and the older 4.x ones (#1289), each once.
    #[test]
    fn the_dropdown_offers_older_models_too() {
        for model in ["claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"] {
            assert!(MODELS.contains(&model), "{model} missing from the dropdown");
        }
        assert_eq!(MODELS[0], DEFAULT_MODEL, "the default stays first");
        let mut seen = std::collections::BTreeSet::new();
        assert!(MODELS.iter().all(|m| seen.insert(*m)), "a model is listed twice");
    }

    /// #1289: each listed model gets only the fields it takes. A field it doesn't take is a 400, so a
    /// request built for the newest model can't be sent to an older one unchanged.
    #[test]
    fn every_listed_model_gets_a_body_it_takes() {
        let msgs = vec![Msg::user_text("hi")];
        let tools = vec![ToolDef { name: "t".into(), description: "d".into(), schema: json!({"type": "object"}) }];
        for model in MODELS.iter().copied().chain(["claude-haiku-4-5-20251001", "claude-opus-4-5-20251101"]) {
            let profile = profile_for(model);
            let client = Anthropic::new(None, "sk-test".into(), model);
            let body = client.body(&TurnRequest { system: "s", messages: &msgs, tools: &tools, effort: Effort::Xhigh, max_tokens: MAX_TOKENS });
            assert_eq!(body.get("thinking").is_some(), profile.thinking == Thinking::Adaptive, "{model} thinking");
            assert_eq!(body.get("output_config").is_some(), profile.effort.is_some(), "{model} effort");
            assert_eq!(body.get("fallbacks").is_some(), profile.fallbacks, "{model} fallbacks");
            assert_eq!(client.headers().iter().any(|(k, _)| *k == "anthropic-beta"), profile.fallbacks, "{model} beta header");
            assert_eq!(body["tools"][0].get("eager_input_streaming").is_some(), profile.eager_input, "{model} eager");
            if let Some(levels) = profile.effort {
                assert!(levels.contains(&effort_for(model, Effort::Xhigh).unwrap_or(Effort::Low)), "{model} sends a level it lacks");
            }
        }
    }

    /// Haiku 4.5 takes only a manual thinking budget and no effort parameter: it gets neither, so
    /// the request runs as the model does by default, without thinking.
    #[test]
    fn haiku_45_gets_no_thinking_no_effort_and_no_fallback() {
        let client = Anthropic::new(None, "sk-test".into(), "claude-haiku-4-5");
        let tools = vec![ToolDef { name: "t".into(), description: "d".into(), schema: json!({"type": "object"}) }];
        let body = client.body(&TurnRequest { system: "s", messages: &[], tools: &tools, effort: Effort::High, max_tokens: MAX_TOKENS });
        for field in ["thinking", "output_config", "fallbacks"] {
            assert!(body.get(field).is_none(), "{field} must not be sent to Haiku 4.5");
        }
        assert!(body["tools"][0].get("eager_input_streaming").is_none());
        assert!(client.headers().iter().all(|(k, _)| *k != "anthropic-beta"));
    }

    /// Haiku 5.5 thinks adaptively, but has no server-side fallback, so the field is left out.
    #[test]
    fn haiku_55_takes_adaptive_thinking_but_no_fallback() {
        let client = Anthropic::new(None, "sk-test".into(), "claude-haiku-5-5");
        let body = client.body(&TurnRequest { system: "s", messages: &[], tools: &[], effort: Effort::Medium, max_tokens: MAX_TOKENS });
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert!(body.get("fallbacks").is_none());
        assert!(client.headers().iter().all(|(k, _)| *k != "anthropic-beta"));
    }

    /// The newest Opus keeps the fallback field, and an AI that turns fallbacks off drops it everywhere.
    #[test]
    fn opus_55_keeps_the_fallback_and_an_ai_can_turn_it_off() {
        let mut client = Anthropic::new(None, "sk-test".into(), "claude-opus-5-5");
        let body = client.body(&TurnRequest { system: "s", messages: &[], tools: &[], effort: Effort::High, max_tokens: MAX_TOKENS });
        assert_eq!(body["fallbacks"], "default");
        client.fallbacks = false;
        assert!(client.body(&TurnRequest { system: "s", messages: &[], tools: &[], effort: Effort::High, max_tokens: 10 }).get("fallbacks").is_none());
    }

    /// The longest id wins, a date suffix matches, and a longer id that only starts the same way doesn't.
    #[test]
    fn profiles_match_by_whole_id() {
        assert_eq!(profile_for("claude-opus-5-5").id, "claude-opus-5-5");
        assert_eq!(profile_for("claude-opus-5-5-20260101").id, "claude-opus-5-5", "not claude-opus-5");
        assert_eq!(profile_for("claude-opus-5").id, "claude-opus-5");
        assert_eq!(profile_for("claude-haiku-4-5-20251001").id, "claude-haiku-4-5");
        assert_eq!(profile_for("anthropic.claude-sonnet-4-6").id, "claude-sonnet-4-6");
        assert_eq!(profile_for("claude-opus-4-65").id, "", "an id that only starts like a known one is unknown");
        assert_eq!(profile_for("claude-opus-4-65").thinking, Thinking::Adaptive);
    }

    /// Effort is sent only where the model takes it, at the nearest level it has.
    #[test]
    fn effort_is_sent_only_where_the_model_takes_it() {
        assert_eq!(effort_for("claude-haiku-4-5", Effort::High), None);
        assert_eq!(effort_for("claude-sonnet-4-5", Effort::High), None);
        assert_eq!(effort_for("claude-opus-4-5", Effort::Max), Some(Effort::High), "Opus 4.5 stops at high");
        assert_eq!(effort_for("claude-opus-4-7", Effort::Xhigh), Some(Effort::Xhigh), "4.7 has xhigh");
        assert_eq!(effort_for("claude-sonnet-4-6", Effort::Xhigh), Some(Effort::High), "4.6 has no xhigh");
    }

    /// Every level goes to a model that has it; one that lacks it gets the nearest level below.
    #[test]
    fn effort_steps_down_to_a_level_the_model_has() {
        for e in Effort::ALL {
            assert_eq!(effort_for("claude-opus-5-5", e), Some(e));
            assert_eq!(Effort::parse(&e.as_str().to_uppercase()), Some(e));
        }
        for model in ["claude-opus-5-5", "claude-sonnet-4-6", "anthropic/claude-future", "anthropic.claude-future"] {
            let client = Anthropic::new(None, "sk-test".into(), model);
            for effort in [Effort::None, Effort::Minimal] {
                assert_eq!(effort_for(model, effort), Some(Effort::Low));
                let body = client.body(&TurnRequest { system: "s", messages: &[], tools: &[], effort, max_tokens: 10 });
                assert_eq!(body["output_config"]["effort"], "low");
                assert_eq!(Effort::parse(effort.as_str()), Some(effort));
            }
        }
        assert_eq!(effort_for("claude-sonnet-4-6", Effort::Xhigh), Some(Effort::High));
        assert_eq!(effort_for("anthropic/claude-opus-4-6", Effort::Max), Some(Effort::Max));
        assert_eq!(effort_for("claude-opus-4-5-20251101", Effort::Max), Some(Effort::High));
        assert_eq!(Effort::Low.within(&[Effort::Medium, Effort::High]), Effort::Medium, "none below: the lowest it has");
        assert_eq!(Effort::Max.within(&[]), Effort::Max);
        assert_eq!(Effort::parse("extreme"), None);
        let a = Anthropic::new(None, "k".into(), "claude-sonnet-4-6");
        let b = a.body(&TurnRequest { system: "s", messages: &[], tools: &[], effort: Effort::Xhigh, max_tokens: 10 });
        assert_eq!(b["output_config"]["effort"], "high");
    }

    #[test]
    fn stream_state_builds_blocks() {
        let events = [
            json!({"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":10,"cache_read_input_tokens":4}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"x","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ];
        let mut st = StreamState::default();
        let mut thinking = String::new();
        for e in events {
            st.apply(&SseEvent { event: None, data: e.to_string() }, &mut |d| {
                if let Delta::Thinking(t) = d {
                    thinking.push_str(&t)
                }
            })
            .unwrap();
        }
        let r = st.finish("m").unwrap();
        assert_eq!(thinking, "hmm");
        assert_eq!(r.stop, StopReason::ToolUse);
        assert_eq!(r.usage, Usage { input_tokens: 10, output_tokens: 7, cache_read_tokens: 4, cache_write_tokens: 0 });
        assert_eq!(r.message.blocks[0], Block::Thinking { thinking: "hmm".into(), signature: "sig".into() });
        assert_eq!(r.message.tool_uses(), vec![("t1".into(), "x".into(), json!({"a": 1}))]);
    }

    #[test]
    fn stream_errors_and_truncation() {
        let mut st = StreamState::default();
        let e = st
            .apply(
                &SseEvent { event: Some("error".into()), data: json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}).to_string() },
                &mut |_| {},
            )
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::EngineMissing);
        let st = StreamState::default();
        assert_eq!(st.finish("m").unwrap_err().code, ErrorCode::Truncated);
        let mut st = StreamState::default();
        assert!(st.apply(&SseEvent { event: None, data: "{not json".into() }, &mut |_| {}).is_err());
        // indexes far out of range are ignored, not allocated
        st.apply(
            &SseEvent { event: None, data: json!({"type":"content_block_start","index":999999999,"content_block":{"type":"text","text":""}}).to_string() },
            &mut |_| {},
        )
        .unwrap();
    }
}
