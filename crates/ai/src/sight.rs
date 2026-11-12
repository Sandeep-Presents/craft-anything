//! Whether a model server's model sees pictures, read before a builder is shown one (R5.4).
//!
//! Each kind of server says it in its own place, so each is a [`SightProbe`]; [`probes`] lists the
//! built-in ones in the order they're asked, and a fork can add its own. They're asked with the
//! same [`Fetch`] and [`Server`] as the context window ([`crate::window`]). A probe answers yes, no,
//! or nothing; the first answer wins, and a model nobody answers for is treated as one that can't
//! see ([`sees`]).
//!
//! A model list says only yes: llama-swap lists `input_modalities: ["text"]` for every model, even
//! one whose own server says it sees, so "text" there is a default, not an answer.

use crate::config::Vision;
use crate::window::{Fetch, Server};
use serde_json::{Value, json};

/// Whether a model sees pictures, and which probe said so (for the journal and `ai vision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sight {
    pub sees: bool,
    pub source: &'static str,
}

/// One way a server says whether its model takes pictures.
pub trait SightProbe: Send + Sync {
    /// A short name for the journal ("llama.cpp").
    fn source(&self) -> &'static str;
    /// `Some(true)` or `Some(false)` when the server says; `None` when it doesn't.
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool>;
}

/// The built-in probes, in the order they're asked. `on_this_network` is false for a remote
/// service, which is asked only through its model list.
pub fn probes(on_this_network: bool) -> Vec<Box<dyn SightProbe>> {
    let mut all: Vec<Box<dyn SightProbe>> = vec![Box::new(ModelList)];
    if on_this_network {
        all.push(Box::new(LlamaSwap));
        all.push(Box::new(LlamaCpp));
        all.push(Box::new(LmStudio));
        all.push(Box::new(Ollama));
    }
    all
}

/// The first answer any probe gives.
pub fn read_sight(server: &Server<'_>, fetch: &dyn Fetch, probes: &[Box<dyn SightProbe>]) -> Option<Sight> {
    probes.iter().find_map(|p| p.sees(server, fetch).map(|sees| Sight { sees, source: p.source() }))
}

/// Ask with the built-in probes. As with the window ([`crate::window::ask`]), a model llama-swap
/// lists but hasn't loaded is asked about only through the list, since any other question would
/// start it.
pub fn ask(server: &Server<'_>, fetch: &dyn Fetch, on_this_network: bool) -> Option<Sight> {
    read_sight(server, fetch, &probes(on_this_network && !crate::window::would_start_it(server, fetch)))
}

/// Ask the server with `headers` (its key, when it has one). Each address is asked once.
pub fn read_from_server(server: &Server<'_>, headers: &[(&'static str, String)], on_this_network: bool) -> Option<Sight> {
    ask(server, &crate::window::HttpFetch::new(headers), on_this_network)
}

/// Whether a model is shown pictures with this setting, given what its server said.
pub fn sees(setting: Vision, read: Option<Sight>) -> bool {
    match setting {
        Vision::Yes => true,
        Vision::No => false,
        Vision::Auto => read.is_some_and(|s| s.sees),
    }
}

/// `architecture.input_modalities` (OpenRouter, llama-swap) or `architecture.modality`
/// (`"text+image->text"`) in an OpenAI-style model list: only a yes counts.
struct ModelList;

impl SightProbe for ModelList {
    fn source(&self) -> &'static str {
        "model list"
    }
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool> {
        let m = server.listed(fetch)?;
        let arch = m.get("architecture")?;
        let listed = arch.get("input_modalities").and_then(Value::as_array).is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("image")));
        let modality =
            arch.get("modality").and_then(Value::as_str).and_then(|m| m.split("->").next()).is_some_and(|input| input.split('+').any(|k| k.trim() == "image"));
        (listed || modality).then_some(true)
    }
}

/// llama.cpp's `/props` says `modalities.vision` when it has a vision projector loaded (older
/// servers have no `modalities`, which says nothing).
fn props_vision(props: &Value) -> Option<bool> {
    props.get("modalities")?.get("vision")?.as_bool()
}

/// llama-swap passes `/upstream/<model>/props` to the model's own server. Asked only when its
/// list says the model is loaded: asking an unloaded one would start it.
struct LlamaSwap;

impl SightProbe for LlamaSwap {
    fn source(&self) -> &'static str {
        "llama-swap"
    }
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool> {
        let id = crate::window::loaded_swap_model(server, fetch)?;
        props_vision(&fetch.get(&format!("{}/upstream/{id}/props", server.root()))?)
    }
}

/// llama.cpp's server, asked about the model by name (a router serving several needs it).
struct LlamaCpp;

impl SightProbe for LlamaCpp {
    fn source(&self) -> &'static str {
        "llama.cpp"
    }
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool> {
        props_vision(&fetch.get(&format!("{}/props?model={}", server.root(), crate::window::query_text(server.model)))?)
    }
}

/// LM Studio's own API: a model's `type` is `vlm` when it takes pictures, `llm` when it doesn't.
struct LmStudio;

impl SightProbe for LmStudio {
    fn source(&self) -> &'static str {
        "LM Studio"
    }
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool> {
        match fetch.get(&format!("{}/api/v0/models/{}", server.root(), server.model))?.get("type")?.as_str()? {
            "vlm" => Some(true),
            "llm" => Some(false),
            _ => None,
        }
    }
}

/// Ollama's `/api/show` lists what a model can do; `vision` is among them when it takes pictures.
/// It answers for a model that isn't loaded without loading it.
struct Ollama;

impl SightProbe for Ollama {
    fn source(&self) -> &'static str {
        "Ollama"
    }
    fn sees(&self, server: &Server<'_>, fetch: &dyn Fetch) -> Option<bool> {
        let shown = fetch.post(&format!("{}/api/show", server.root()), &json!({"model": server.model}))?;
        let can = shown.get("capabilities")?.as_array()?;
        Some(can.iter().any(|c| c.as_str() == Some("vision")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Answers from a table of URLs (GET) and `POST url` keys; anything else is nothing.
    struct Recorded(BTreeMap<String, Value>);

    impl Fetch for Recorded {
        fn get(&self, url: &str) -> Option<Value> {
            self.0.get(url).cloned()
        }
        fn post(&self, url: &str, body: &Value) -> Option<Value> {
            self.0.get(&format!("POST {url} {body}")).cloned()
        }
    }

    fn recorded(pairs: &[(&str, Value)]) -> Recorded {
        Recorded(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    const BASE: &str = "http://127.0.0.1:8080/v1";

    fn said(model: &str, fetch: &Recorded) -> Option<Sight> {
        ask(&Server { base: BASE, model }, fetch, true)
    }

    #[test]
    fn llama_swap_lists_text_for_a_model_whose_own_server_sees() {
        // The shapes a live llama-swap server sent: its list says text for every model, while the
        // loaded model's own server says it sees.
        let entry = |id: &str, model_id: Option<&str>, status: &str| {
            let mut swap = json!({"aliases": []});
            if let Some(m) = model_id {
                swap["modelID"] = json!(m);
            }
            json!({"id": id, "architecture": {"input_modalities": ["text"], "modality": "text->text"}, "meta": {"llamaswap": swap}, "status": {"value": status}})
        };
        let list = json!({"data": [entry("qwen:q8", None, "loaded"), entry("qwen:q8-low", Some("qwen:q8"), "loaded"), entry("gemma:bf16", None, "unloaded")]});
        let fetch = recorded(&[
            ("http://127.0.0.1:8080/v1/models", list),
            ("http://127.0.0.1:8080/upstream/qwen:q8/props", json!({"modalities": {"vision": true}, "default_generation_settings": {"n_ctx": 524288}})),
            // llama-swap would answer this, but only after starting the model and unloading the
            // one in use (seen live), so it must not be asked.
            ("http://127.0.0.1:8080/props?model=gemma%3Abf16", json!({"modalities": {"vision": true}})),
        ]);
        assert_eq!(said("qwen:q8", &fetch), Some(Sight { sees: true, source: "llama-swap" }));
        assert_eq!(said("qwen:q8-low", &fetch), Some(Sight { sees: true, source: "llama-swap" }), "an alias is served by its model");
        // An unloaded model isn't asked (that would start it), and "text" in the list isn't a no.
        assert_eq!(said("gemma:bf16", &fetch), None);
    }

    #[test]
    fn each_server_says_yes_no_or_nothing() {
        let props = "http://127.0.0.1:8080/props?model=m";
        let studio = "http://127.0.0.1:8080/api/v0/models/m";
        let show = "POST http://127.0.0.1:8080/api/show {\"model\":\"m\"}";
        type Case<'a> = (Vec<(&'a str, Value)>, Option<Sight>);
        let cases: Vec<Case> = vec![
            (vec![(props, json!({"modalities": {"vision": true, "audio": false}}))], Some(Sight { sees: true, source: "llama.cpp" })),
            (vec![(props, json!({"modalities": {"vision": false}}))], Some(Sight { sees: false, source: "llama.cpp" })),
            // An older llama.cpp names no modalities: that says nothing, and the next probe is asked.
            (vec![(props, json!({"n_ctx": 4096})), (studio, json!({"type": "vlm"}))], Some(Sight { sees: true, source: "LM Studio" })),
            (vec![(studio, json!({"type": "llm"}))], Some(Sight { sees: false, source: "LM Studio" })),
            (vec![(studio, json!({"type": "embeddings"}))], None),
            (vec![(show, json!({"capabilities": ["completion", "vision", "tools"]}))], Some(Sight { sees: true, source: "Ollama" })),
            (vec![(show, json!({"capabilities": ["completion", "tools"]}))], Some(Sight { sees: false, source: "Ollama" })),
            (vec![(show, json!({"details": {}}))], None),
            // Hostile shapes read as nothing.
            (vec![(props, json!({"modalities": {"vision": "yes"}})), (studio, json!({"type": 3})), (show, json!({"capabilities": "vision"}))], None),
            (vec![], None),
        ];
        for (pairs, expected) in cases {
            assert_eq!(said("m", &recorded(&pairs)), expected, "{pairs:?}");
        }
    }

    #[test]
    fn a_model_list_says_only_yes() {
        let listed = |arch: Value| recorded(&[("http://127.0.0.1:8080/v1/models", json!({"data": [{"id": "m", "architecture": arch}]}))]);
        let yes = Some(Sight { sees: true, source: "model list" });
        assert_eq!(said("m", &listed(json!({"input_modalities": ["text", "image"]}))), yes);
        assert_eq!(said("m", &listed(json!({"modality": "text+image->text"}))), yes);
        for not in [json!({"input_modalities": ["text"]}), json!({"modality": "text->text+image"}), json!({"input_modalities": "image"}), json!(null)] {
            assert_eq!(said("m", &listed(not.clone())), None, "{not}");
        }
        // A remote service is asked only through its list.
        let fetch = recorded(&[("http://127.0.0.1:8080/props?model=m", json!({"modalities": {"vision": true}}))]);
        assert_eq!(read_sight(&Server { base: BASE, model: "m" }, &fetch, &probes(false)), None);
    }

    #[test]
    fn the_setting_decides_and_auto_follows_the_server() {
        let yes = Some(Sight { sees: true, source: "llama.cpp" });
        let no = Some(Sight { sees: false, source: "llama.cpp" });
        for (setting, read, shown) in [
            (Vision::Auto, yes, true),
            (Vision::Auto, no, false),
            (Vision::Auto, None, false),
            (Vision::Yes, None, true),
            (Vision::Yes, no, true),
            (Vision::No, yes, false),
        ] {
            assert_eq!(sees(setting, read), shown, "{setting:?} {read:?}");
        }
    }
}
