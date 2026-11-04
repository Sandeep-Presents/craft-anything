//! Choosing a model from what a service or server lists. The endpoint form and the key cards share
//! it: the person types an address or pastes a key, and once they stop typing the models come
//! (never saved, never on the UI thread), with the likeliest one already chosen.

use crate::i18n::{tr, trf};
use crate::theme::Tokens;
use crate::{CraftApp, Done, widgets};
use craft_anything_ai::{BackendKind, TestReport};
use craft_anything_engine::evidence::ToolError;
use egui::{Ui, Vec2};
use std::collections::BTreeMap;
use std::time::Duration;

/// How long the inputs sit still before the models are asked for.
pub const DEBOUNCE: f64 = 0.6;
/// The last entry of the list: any id, for ones it doesn't offer.
const CUSTOM: &str = "Custom model or alias…";
/// A key shorter than this (without spaces) isn't a key yet.
pub const MIN_KEY: usize = 8;
/// The height of the status line, always reserved so nothing moves when text appears.
pub const STATUS_H: f32 = 18.0;

/// Which control a listing is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// The endpoint form.
    Form,
    /// A card that asks for a key, by the AI's name.
    Card(String),
    /// Settings → Builds: the checkpoints of the ComfyUI server. Its [`Ask`] holds the address in
    /// `base_url` and an empty key (the kind is not used for it).
    Comfy,
}

/// What a listing is asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub kind: BackendKind,
    pub base_url: Option<String>,
    pub key: String,
}

/// What [`ModelChoice::step`] wants done this frame.
#[derive(Debug, PartialEq)]
pub enum Poll {
    Idle,
    /// Look again in this many seconds.
    Wait(f64),
    Fire(Ask),
}

/// The models a service offers and the one picked.
#[derive(Debug, Clone, Default)]
pub struct ModelChoice {
    /// The inputs as of the last frame, and when they last changed (the UI clock, in seconds).
    typed: Option<(Ask, f64)>,
    /// The inputs the list, or the request in flight, is for.
    asked: Option<Ask>,
    /// The request being waited for. A result carrying another number is stale and dropped.
    pub awaiting: Option<u64>,
    pub models: Vec<String>,
    pub error: Option<String>,
    /// The model picked from the list.
    pub picked: String,
    /// "Custom model or alias…" is chosen.
    pub custom: bool,
    /// What was typed for it (also used when there is no list).
    pub typed_model: String,
    /// The service's own default, preselected when it is listed.
    pub default_model: String,
}

impl ModelChoice {
    pub fn loading(&self) -> bool {
        self.awaiting.is_some()
    }

    /// The model to add the AI with: the listed one picked, or what was typed. Empty: none chosen.
    pub fn chosen(&self) -> &str {
        if self.custom || self.models.is_empty() { self.typed_model.trim() } else { self.picked.trim() }
    }

    /// Forget the list (the inputs no longer name anything to ask).
    fn reset(&mut self) {
        self.typed = None;
        self.asked = None;
        self.awaiting = None;
        self.models.clear();
        self.error = None;
        self.picked.clear();
        self.custom = false;
    }

    /// Debounce: the inputs of this frame, `None` when they can't be asked about yet.
    pub fn step(&mut self, now: f64, ask: Option<Ask>) -> Poll {
        let Some(ask) = ask else {
            if self.typed.is_some() || self.asked.is_some() || self.awaiting.is_some() {
                self.reset();
            }
            return Poll::Idle;
        };
        if self.typed.as_ref().map(|(a, _)| a) != Some(&ask) {
            self.typed = Some((ask.clone(), now));
            // The list (or the answer on its way) is for other inputs: drop it, so it can't be
            // added with another server's model. A model typed by hand stays.
            if self.asked.as_ref().is_some_and(|a| *a != ask) {
                self.asked = None;
                self.awaiting = None;
                self.models.clear();
                self.error = None;
                self.picked.clear();
            }
        }
        if self.asked.as_ref() == Some(&ask) {
            return Poll::Idle;
        }
        let due = self.typed.as_ref().map_or(now, |(_, since)| since + DEBOUNCE);
        if now >= due {
            self.asked = Some(ask.clone());
            Poll::Fire(ask)
        } else {
            Poll::Wait(due - now)
        }
    }

    /// A listing came back for the current request.
    pub fn apply(&mut self, result: Result<Vec<String>, ToolError>) {
        self.awaiting = None;
        match result {
            Ok(models) if !models.is_empty() => {
                self.error = None;
                if !models.contains(&self.picked) {
                    self.picked = Some(&self.default_model)
                        .filter(|d| models.contains(d))
                        .cloned()
                        .or_else(|| craft_anything_ai::first_chat_model(&models))
                        .or_else(|| models.first().cloned())
                        .unwrap_or_default();
                }
                self.models = models;
            }
            Ok(_) => {
                self.models.clear();
                self.error = Some(tr("It lists no models yet."));
            }
            Err(e) => {
                self.models.clear();
                self.error = Some(e.message);
            }
        }
    }
}

/// A listing finished: kept only when it answers the request still being waited for.
pub(crate) fn listed(app: &mut CraftApp, slot: &Slot, seq: u64, result: Result<Vec<String>, ToolError>) {
    let choice = choice_mut(app, slot);
    if choice.awaiting == Some(seq) {
        choice.apply(result);
    }
}

pub(crate) fn choice_mut<'a>(app: &'a mut CraftApp, slot: &Slot) -> &'a mut ModelChoice {
    match slot {
        Slot::Form => &mut app.server_form.choice,
        Slot::Card(name) => app.card_models.entry(name.clone()).or_default(),
        Slot::Comfy => &mut app.comfy_form.choice,
    }
}

/// A key someone has pasted: long enough to be one.
pub fn key_looks_real(key: &str) -> bool {
    key.chars().filter(|c| !c.is_whitespace()).count() >= MIN_KEY
}

/// Run one choice's debounce this frame and start the listing when it is due.
pub(crate) fn drive(app: &mut CraftApp, ui: &Ui, slot: &Slot, ask: Option<Ask>, default_model: &str) {
    let now = ui.input(|i| i.time);
    let choice = choice_mut(app, slot);
    choice.default_model = default_model.to_string();
    match choice.step(now, ask) {
        Poll::Idle => {}
        Poll::Wait(s) => ui.ctx().request_repaint_after(Duration::from_secs_f64(s.max(0.02))),
        Poll::Fire(ask) => app.list_models_for(slot.clone(), ask),
    }
}

/// The model control, `width` wide in all: a list of what was found, with a last entry to type any
/// id (the list and the typing field then share the width); with no list, the typing field alone.
/// Read [`ModelChoice::chosen`]. Returns the first control's rect, for lining a label up with it.
pub(crate) fn model_picker(ui: &mut Ui, id_salt: impl std::hash::Hash + std::fmt::Debug, choice: &mut ModelChoice, width: f32) -> egui::Rect {
    let field = |ui: &mut Ui, text: &mut String, name: &str, hint: &str, w: f32| {
        let r = ui.add(widgets::field(ui, text, hint).desired_width(w));
        widgets::named(r, name).rect
    };
    if choice.models.is_empty() {
        return field(ui, &mut choice.typed_model, "Model", "model name", width);
    }
    let gap = ui.spacing().item_spacing.x;
    let combo_w = if choice.custom { ((width - gap) / 2.0).max(60.0) } else { width };
    let shown = if choice.custom { tr(CUSTOM) } else { choice.picked.clone() };
    let (mut pick, mut custom) = (None, false);
    let r = widgets::combo(ui, egui::ComboBox::from_id_salt((&id_salt, "model")).width(combo_w).selected_text(shown), |ui| {
        for m in &choice.models {
            if ui.selectable_label(!choice.custom && *m == choice.picked, m).clicked() {
                pick = Some(m.clone());
            }
        }
        if ui.selectable_label(choice.custom, tr(CUSTOM)).clicked() {
            custom = true;
        }
    });
    let rect = widgets::named(r.response, "Model").rect;
    if let Some(m) = pick {
        choice.picked = m;
        choice.custom = false;
    }
    if custom {
        choice.custom = true;
    }
    if choice.custom {
        field(ui, &mut choice.typed_model, "Custom model", "model name", combo_w);
    }
    rect
}

/// One line that always keeps its height: the models being looked for, how many were found, or
/// why not; `test` (a test's outcome) takes the line when there is one.
pub(crate) fn status_line(ui: &mut Ui, choice: &ModelChoice, test: Option<&TestLine>) {
    let width = ui.available_width();
    status_line_w(ui, choice, test, width, false);
}

/// [`status_line`] in a given width, so it can share a row with buttons. `checkpoints` counts what
/// was found as ComfyUI checkpoints rather than models.
pub(crate) fn status_line_w(ui: &mut Ui, choice: &ModelChoice, test: Option<&TestLine>, width: f32, checkpoints: bool) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width.max(0.0), STATUS_H), egui::Sense::hover());
    let mut line = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let small = |text: String, color| egui::Label::new(egui::RichText::new(text).size(crate::theme::CAPTION).color(color)).truncate();
    let loading = |ui: &mut Ui, text: &str| {
        ui.add(widgets::Spinner::new().size(crate::theme::CAPTION).color(t.accent));
        ui.add(small(tr(text), t.text_faint));
    };
    match test {
        Some(TestLine::Running) => loading(&mut line, "Testing… a model that isn't loaded yet can take a minute"),
        Some(TestLine::Said(words)) => {
            line.add(small(words.clone(), t.success)).on_hover_text(words);
        }
        Some(TestLine::Answered(s)) => {
            line.add(small(trf("Answered in {seconds} s", &[("seconds", &format!("{s:.1}"))]), t.success));
        }
        Some(TestLine::Failed(e)) => {
            line.add(small(e.clone(), t.danger)).on_hover_text(e);
        }
        None if choice.loading() => loading(&mut line, if checkpoints { "Looking for checkpoints…" } else { "Looking for models…" }),
        None => {
            if let Some(e) = &choice.error {
                line.add(small(e.clone(), t.danger)).on_hover_text(e);
            } else if choice.models.len() == 1 {
                line.add(small(tr(if checkpoints { "1 checkpoint found" } else { "1 model found" }), t.text_faint));
            } else if !choice.models.is_empty() {
                let words = if checkpoints { "{n} checkpoints found" } else { "{n} models found" };
                line.add(small(trf(words, &[("n", &choice.models.len().to_string())]), t.text_faint));
            }
        }
    }
}

/// What a test of the form's endpoint says.
#[derive(Debug, Clone, PartialEq)]
pub enum TestLine {
    Running,
    Answered(f64),
    /// Passed, in these words (the ComfyUI check: its version, card and checkpoints).
    Said(String),
    Failed(String),
}

/// What Settings → Builds holds for ComfyUI: the checkpoints its server lists (picked or typed)
/// and the last Test.
#[derive(Debug, Clone, Default)]
pub struct ComfyForm {
    pub choice: ModelChoice,
    pub test: Option<FormTest>,
}

/// A test of the form's endpoint: which request it is, what it was for, and how it went.
#[derive(Debug, Clone)]
pub struct FormTest {
    pub seq: u64,
    /// A fingerprint of what was tried; the line is hidden once the form no longer matches it.
    pub signature: u64,
    pub line: TestLine,
}

/// A saved AI's test, shown under its row.
#[derive(Debug, Clone)]
pub enum RowTest {
    Running,
    Answered(TestReport),
    Failed(String),
}

/// The words under a row for a finished test.
pub fn row_test_words(r: &TestReport) -> String {
    if r.version_only {
        return trf("Installed: {reply}", &[("reply", &r.reply)]);
    }
    let reply: String = r.reply.chars().take(60).collect();
    trf("Answered in {seconds} s: {reply}", &[("seconds", &format!("{:.1}", r.latency_ms as f64 / 1000.0)), ("reply", &reply)])
}

/// The models of every key card, by AI name.
pub type CardModels = BTreeMap<String, ModelChoice>;

impl CraftApp {
    /// Ask a service or server for its models, off the UI thread, tagged so only the latest
    /// request for its control is kept.
    pub fn list_models_for(&mut self, slot: Slot, ask: Ask) {
        self.model_seq = self.model_seq.wrapping_add(1);
        let seq = self.model_seq;
        let choice = choice_mut(self, &slot);
        choice.awaiting = Some(seq);
        choice.error = None;
        if slot == Slot::Comfy {
            let (engine, address) = (self.engine.clone(), ask.base_url.unwrap_or_default());
            self.jobs.spawn(tr("List ComfyUI's checkpoints"), false, move |_| {
                let result = comfy_check(&engine, &address).map(|v| strings(&v["checkpoints"]));
                Ok(Done::ModelsListed { slot, seq, result })
            });
            return;
        }
        let hub = self.hub.clone();
        self.jobs.spawn(tr("List an AI's models"), false, move |_| {
            let key = Some(ask.key.clone()).filter(|k| !k.trim().is_empty());
            let result = hub.list_models_at(ask.kind, ask.base_url.as_deref(), key);
            Ok(Done::ModelsListed { slot, seq, result })
        });
    }

    /// Try the endpoint in the form with a tiny request, without adding it.
    pub fn test_endpoint(&mut self) {
        let Some((config, key)) = crate::settings_ui::form_config(&self.server_form) else { return };
        self.model_seq = self.model_seq.wrapping_add(1);
        let seq = self.model_seq;
        let signature = crate::settings_ui::form_signature(&self.server_form);
        self.server_form.test = Some(FormTest { seq, signature, line: TestLine::Running });
        let hub = self.hub.clone();
        self.jobs.spawn(tr("Test an AI"), false, move |_| {
            let result = hub.test_config(&config, key);
            Ok(Done::EndpointTested { seq, result })
        });
    }

    /// Ask the ComfyUI server in Settings → Builds for its version, card and checkpoints; the line
    /// beside the Test button shows the answer.
    pub fn test_comfyui(&mut self) {
        let address = self.settings.assets.comfyui.address.trim().to_string();
        self.model_seq = self.model_seq.wrapping_add(1);
        let seq = self.model_seq;
        self.comfy_form.test = Some(FormTest { seq, signature: comfy_signature(&address), line: TestLine::Running });
        let engine = self.engine.clone();
        self.jobs.spawn(tr("Test ComfyUI"), false, move |_| {
            let result = comfy_check(&engine, &address).map(|v| v["words"].as_str().unwrap_or_default().to_string());
            Ok(Done::ComfyTested { seq, result })
        });
    }

    /// Try a saved AI with a tiny request; its row shows the answer.
    pub fn test_backend(&mut self, name: &str) {
        self.row_tests.insert(name.to_string(), RowTest::Running);
        let (hub, name) = (self.hub.clone(), name.to_string());
        self.jobs.spawn(tr("Test an AI"), false, move |_| {
            let result = hub.test_backend(&name);
            Ok(Done::BackendTested { name, result })
        });
    }
}

/// What a ComfyUI test is for, so its line hides once the address changes.
pub(crate) fn comfy_signature(address: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    address.trim().hash(&mut h);
    h.finish()
}

/// What Settings → Builds asks the ComfyUI server for its checkpoints with: only a web address
/// with a host is worth asking.
pub(crate) fn comfy_ask(address: &str) -> Option<Ask> {
    let address = craft_anything_engines::assets::comfyui::clean_address(address).ok()?;
    Some(Ask { kind: BackendKind::OpenaiCompatible, base_url: Some(address), key: String::new() })
}

/// The engine's `assets_comfyui_check` tool, called off the UI thread.
fn comfy_check(engine: &craft_anything_engine::Engine, address: &str) -> Result<serde_json::Value, ToolError> {
    let opts = craft_anything_engine::CallOptions::new(craft_anything_engine::Caller::Ui);
    engine.call("assets_comfyui_check", serde_json::json!({"address": address}), opts).map(|o| o.to_json().get("result").cloned().unwrap_or_default())
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// The ComfyUI test finished.
pub(crate) fn comfy_tested(app: &mut CraftApp, seq: u64, result: Result<String, ToolError>) {
    if let Some(test) = app.comfy_form.test.as_mut().filter(|t| t.seq == seq) {
        test.line = match result {
            Ok(words) => TestLine::Said(words),
            Err(e) => TestLine::Failed(e.message),
        };
    }
}

/// The endpoint test finished.
pub(crate) fn endpoint_tested(app: &mut CraftApp, seq: u64, result: Result<TestReport, ToolError>) {
    if let Some(test) = app.server_form.test.as_mut().filter(|t| t.seq == seq) {
        test.line = match result {
            Ok(r) => TestLine::Answered(r.latency_ms as f64 / 1000.0),
            Err(e) => TestLine::Failed(e.message),
        };
    }
}

/// A saved AI's test finished.
pub(crate) fn backend_tested(app: &mut CraftApp, name: String, result: Result<TestReport, ToolError>) {
    app.row_tests.insert(name, result.map_or_else(|e| RowTest::Failed(e.message), RowTest::Answered));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(key: &str) -> Ask {
        Ask { kind: BackendKind::OpenaiCompatible, base_url: Some("http://127.0.0.1:9/v1".into()), key: key.into() }
    }

    fn models(ids: &[&str]) -> Result<Vec<String>, ToolError> {
        Ok(ids.iter().map(|s| s.to_string()).collect())
    }

    /// Editing keeps the wait going; only a pause of 600 ms fires, once per set of inputs.
    #[test]
    fn a_listing_waits_for_the_typing_to_stop() {
        let mut c = ModelChoice::default();
        assert!(matches!(c.step(0.0, Some(ask("a"))), Poll::Wait(_)));
        assert!(matches!(c.step(0.5, Some(ask("ab"))), Poll::Wait(w) if w > 0.5), "an edit starts the wait again");
        assert!(matches!(c.step(1.0, Some(ask("ab"))), Poll::Wait(_)));
        assert_eq!(c.step(1.2, Some(ask("ab"))), Poll::Fire(ask("ab")));
        assert_eq!(c.step(1.3, Some(ask("ab"))), Poll::Idle, "asked once");
        assert!(matches!(c.step(1.4, Some(ask("abc"))), Poll::Wait(_)), "a new key asks again");
    }

    /// New inputs drop the old list and the answer still on its way; a typed model stays.
    #[test]
    fn new_inputs_drop_the_old_list_and_its_late_answer() {
        let mut c = ModelChoice::default();
        c.step(0.0, Some(ask("a")));
        assert_eq!(c.step(1.0, Some(ask("a"))), Poll::Fire(ask("a")));
        c.awaiting = Some(1);
        c.apply(models(&["old-model"]));
        c.step(2.0, Some(ask("a")));
        assert_eq!(c.step(3.0, Some(ask("a"))), Poll::Idle);
        // Asked again for the same inputs, then they change while the answer is on its way.
        c.typed_model = "my-alias".into();
        c.custom = true;
        c.awaiting = Some(2);
        assert!(matches!(c.step(4.0, Some(ask("b"))), Poll::Wait(_)));
        assert!(c.models.is_empty() && !c.loading() && c.picked.is_empty(), "{c:?}");
        assert_eq!(c.chosen(), "my-alias", "a typed model stays");
        assert_eq!(c.step(5.0, Some(ask("b"))), Poll::Fire(ask("b")), "the new inputs are asked about");
    }

    /// Inputs that stop naming something to ask about empty the list.
    #[test]
    fn losing_the_address_forgets_the_list() {
        let mut c = ModelChoice::default();
        c.step(0.0, Some(ask("")));
        assert!(matches!(c.step(1.0, Some(ask(""))), Poll::Fire(_)));
        c.awaiting = Some(1);
        c.apply(models(&["a-model"]));
        assert_eq!(c.chosen(), "a-model");
        assert_eq!(c.step(2.0, None), Poll::Idle);
        assert!(c.models.is_empty() && !c.loading() && c.chosen().is_empty());
    }

    /// The service's own model wins when listed; otherwise the first that can chat.
    #[test]
    fn the_likeliest_model_is_preselected() {
        let mut c = ModelChoice { default_model: "gpt-5".into(), ..Default::default() };
        c.apply(models(&["text-embedding-3", "gpt-5", "gpt-4"]));
        assert_eq!(c.picked, "gpt-5");
        let mut c = ModelChoice { default_model: "gone".into(), ..Default::default() };
        c.apply(models(&["nomic-embed-text", "qwen3:8b"]));
        assert_eq!(c.picked, "qwen3:8b");
        c.picked = "qwen3:8b".into();
        c.apply(models(&["nomic-embed-text", "qwen3:8b", "x"]));
        assert_eq!(c.picked, "qwen3:8b", "a pick that is still offered stays");
    }

    /// A failed listing leaves the typing field, and what was typed is the model.
    #[test]
    fn a_failed_listing_falls_back_to_typing() {
        let mut c = ModelChoice::default();
        c.apply(Err(ToolError::invalid("nothing is listening")));
        assert_eq!(c.error.as_deref(), Some("nothing is listening"));
        c.typed_model = " my-alias ".into();
        assert_eq!(c.chosen(), "my-alias");
        c.apply(models(&["a", "b"]));
        c.custom = true;
        assert_eq!(c.chosen(), "my-alias", "the custom entry uses what was typed");
    }

    #[test]
    fn a_key_needs_eight_characters() {
        assert!(!key_looks_real("  sk-abc  "));
        assert!(key_looks_real("sk-abcdef"));
        assert!(!key_looks_real("        x"));
    }

    #[test]
    fn a_row_test_says_what_ran() {
        let mut r = TestReport {
            backend: "A".into(),
            model: "m".into(),
            latency_ms: 1250,
            reply: "r".repeat(80),
            version_only: false,
            program: None,
            saved_program_replaced: None,
        };
        let words = row_test_words(&r);
        assert!(words.starts_with("Answered in 1.2 s: ") || words.starts_with("Answered in 1.3 s: "), "{words}");
        assert!(words.ends_with(&"r".repeat(60)) && !words.ends_with(&"r".repeat(61)), "{words}");
        r.version_only = true;
        r.reply = "opencode 1.2".into();
        assert_eq!(row_test_words(&r), "Installed: opencode 1.2");
    }
}
