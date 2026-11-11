//! Experimental scene-bundle client. Parsing and picking dispatch through the shared engine.
pub mod camera;
mod gpu;
use crate::{
    jobs::Jobs,
    theme::{self, ThemeKind, Tokens},
};
use craft_anything_engine::{CallOptions, Caller, Engine, Session, Settings, tools::scene_tools::LoadedScene};
use craft_anything_formats::scene_bundle::{Visibility, math::Bounds};
use eframe::egui_wgpu::RenderState;
use egui::{Sense, TextureId};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ViewState {
    pub camera: camera::Camera,
    pub visibility: Visibility,
    pub selected_instance: Option<usize>,
    pub selected_resource: Option<usize>,
    pub search: String,
}
enum Done {
    Loaded(Arc<LoadedScene>, Box<gpu::Gpu>),
    Rendered(Box<gpu::Gpu>),
    Picked { scene_id: String, revision: u64, selected: Option<usize>, complete: bool },
}
pub struct SceneApp {
    pub state: ViewState,
    pub path: String,
    pub status: String,
    pub model: Option<Arc<LoadedScene>>,
    engine: Arc<Engine>,
    graphics: Option<RenderState>,
    jobs: Jobs<Done>,
    gpu: Option<Box<gpu::Gpu>>,
    texture: Option<TextureId>,
    rows: Vec<(usize, usize)>,
    dirty: bool,
    revision: u64,
    size: [u32; 2],
    initial_open: bool,
}
impl SceneApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: String) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, ThemeKind::Studio, false);
        Self {
            state: ViewState::default(),
            initial_open: !path.is_empty(),
            path,
            status: "Open a scene.json bundle to start.".into(),
            model: None,
            engine: Arc::new(Engine::new(Session::new(Settings { deep_mode: true, ..Default::default() }))),
            graphics: cc.wgpu_render_state.clone(),
            jobs: Jobs::default(),
            gpu: None,
            texture: None,
            rows: Vec::new(),
            dirty: false,
            revision: 0,
            size: [1000, 700],
        }
    }
    pub fn ready(&self) -> bool {
        self.texture.is_some() && self.gpu.is_some() && !self.jobs.busy() && !self.dirty
    }
    /// The last worker error, for verification clients that must fail promptly.
    pub fn error(&self) -> Option<&str> {
        self.jobs.list.iter().rev().find_map(|j| j.outcome.as_ref().and_then(|v| v.as_ref().err()).map(String::as_str))
    }
    pub fn changed(&mut self) {
        self.dirty = true;
        self.revision = self.revision.saturating_add(1);
    }
    fn open(&mut self) {
        if self.jobs.busy() {
            return;
        }
        let Some(graphics) = self.graphics.clone() else {
            self.status = "A graphics adapter is required for the viewport. The inspection tools work headlessly.".into();
            return;
        };
        let path = self.path.clone();
        let engine = self.engine.clone();
        self.status = "Loading and validating scene…".into();
        self.jobs.spawn("Open scene", true, move |j| {
            let report = j.reporter();
            engine
                .call("formats_scene_bundle_open", json!({"path":path}), CallOptions::new(Caller::Ui).with_cancel(j.cancel.clone()).with_progress(&report))
                .map_err(|e| e.message)?;
            let model = engine.session().ext.get::<LoadedScene>().ok_or("The loaded scene is unavailable")?;
            let gpu = gpu::Gpu::new(&graphics, &model.scene, &j.cancel)?;
            Ok(Done::Loaded(model, Box::new(gpu)))
        });
    }
    fn poll(&mut self) {
        for done in self.jobs.poll() {
            match done.result {
                Ok(Done::Loaded(model, gpu)) => {
                    self.state = ViewState::default();
                    self.state.camera.fit(model.scene.bounds);
                    self.rows = resource_rows(&model.scene.resources);
                    self.status = format!("{} mesh instances · {} resources", model.scene.instances.len(), model.scene.resources.len());
                    self.model = Some(model);
                    self.gpu = Some(gpu);
                    self.changed();
                }
                Ok(Done::Rendered(gpu)) => {
                    if let Some(graphics) = &self.graphics {
                        let mut renderer = graphics.renderer.write();
                        if let Some(old) = self.texture.take() {
                            renderer.free_texture(&old);
                        }
                        self.texture = Some(renderer.register_native_texture(&graphics.device, &gpu.view, eframe::wgpu::FilterMode::Linear));
                    }
                    self.gpu = Some(gpu);
                }
                Ok(Done::Picked { scene_id, revision, selected, complete }) => {
                    if self.model.as_ref().is_some_and(|m| m.scene.id == scene_id) && self.revision == revision {
                        if complete {
                            self.state.selected_instance = selected;
                            self.state.selected_resource = None;
                            self.status = if selected.is_some() { "Surface selected" } else { "No visible surface at that point" }.into();
                            self.changed();
                        } else {
                            self.status = "Selection exceeded its work budget or encountered a singular transform; no certain hit.".into();
                        }
                    }
                }
                Err(e) => {
                    self.status = e;
                }
            }
        }
    }
    pub fn pick(&mut self, ndc: [f64; 2], aspect: f64) {
        if self.jobs.busy() {
            return;
        }
        let Some(model) = self.model.clone() else {
            return;
        };
        let (origin, direction) = self.state.camera.ray(ndc, aspect);
        let f = &self.state.visibility;
        let resources = |ids: &BTreeSet<usize>| ids.iter().filter_map(|id| model.scene.resources.get(*id)).map(|r| r.guid.clone()).collect::<Vec<_>>();
        let instances = |ids: &BTreeSet<usize>| ids.iter().filter_map(|id| model.scene.instances.get(*id)).map(|i| i.id.clone()).collect::<Vec<_>>();
        let params = json!({"scene_id":model.scene.id,"origin":origin,"direction":direction,"hidden_resources":resources(&f.hidden_resources),"isolate_resources":resources(&f.isolate_resources),"hidden_instances":instances(&f.hidden_instances),"isolate_instances":instances(&f.isolate_instances)});
        let engine = self.engine.clone();
        let revision = self.revision;
        self.jobs.spawn("Select surface", true, move |j| {
            let result =
                engine.call("formats_scene_surface_pick", params, CallOptions::new(Caller::Ui).with_cancel(j.cancel.clone())).map_err(|e| e.message)?.to_json();
            let result = result.get("result").ok_or("Selection result is absent")?;
            let hit = result.get("pick").ok_or("Selection details are absent")?;
            let selected = hit.get("instance").and_then(|v| v.as_u64()).and_then(|v| usize::try_from(v).ok());
            Ok(Done::Picked { scene_id: model.scene.id.clone(), revision, selected, complete: hit.get("complete").and_then(|v| v.as_bool()).unwrap_or(false) })
        });
    }
    fn resource_set(&self, id: usize) -> BTreeSet<usize> {
        let mut out = BTreeSet::from([id]);
        if let Some(model) = &self.model {
            let ids = model.scene.resources.iter().enumerate().map(|(id, r)| (r.guid.as_str(), id)).collect::<BTreeMap<_, _>>();
            for (child, _) in &self.rows {
                if model.scene.resources.get(*child).and_then(|r| r.parent_guid.as_ref()).is_some_and(|p| ids.get(p.as_str()).is_some_and(|i| out.contains(i)))
                {
                    out.insert(*child);
                }
            }
        }
        out
    }
    fn selection_bounds(&self) -> Option<Bounds> {
        let model = self.model.as_ref()?;
        if let Some(instance) = self.state.selected_instance {
            return model.scene.instances.get(instance).map(|i| i.bounds);
        }
        let resources = self.resource_set(self.state.selected_resource?);
        let mut bounds = Bounds::empty();
        for i in &model.scene.instances {
            if i.resources.iter().any(|r| resources.contains(r)) {
                bounds.include(i.bounds.0[0]);
                bounds.include(i.bounds.0[1]);
            }
        }
        bounds.valid().then_some(bounds)
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Resources");
        ui.add(egui::TextEdit::singleline(&mut self.state.search).hint_text("Find a resource"));
        let Some(model) = self.model.clone() else {
            ui.label("No scene loaded");
            return;
        };
        ui.label(format!("{} definitions · {} instances", model.scene.meshes.len(), model.scene.instances.len()));
        ui.separator();
        let query = self.state.search.to_lowercase();
        let rows = self
            .rows
            .iter()
            .copied()
            .filter(|(id, _)| model.scene.resources.get(*id).is_some_and(|r| query.is_empty() || r.name.to_lowercase().contains(&query)))
            .collect::<Vec<_>>();
        egui::ScrollArea::vertical().max_height((ui.available_height() - 235.).max(120.)).show_rows(ui, 22., rows.len(), |ui, range| {
            for row in range {
                if let Some((id, depth)) = rows.get(row).copied()
                    && let Some(r) = model.scene.resources.get(id)
                {
                    ui.horizontal(|ui| {
                        ui.add_space((depth.min(16) * 10) as f32);
                        let hidden = self.state.visibility.hidden_resources.contains(&id);
                        let label = format!(
                            "{}{} ({})",
                            if hidden { "○ " } else { "" },
                            r.name.chars().take(120).collect::<String>(),
                            model.scene.resource_instance_counts.get(id).copied().unwrap_or(0)
                        );
                        if ui.selectable_label(self.state.selected_resource == Some(id), label).on_hover_text(&r.name).clicked() {
                            self.state.selected_resource = Some(id);
                            self.state.selected_instance = None;
                            self.changed();
                        }
                    });
                }
            }
        });
        ui.separator();
        if let Some(id) = self.state.selected_instance
            && let Some(i) = model.scene.instances.get(id)
        {
            ui.label("Selected mesh instance");
            ui.monospace(&i.entity_guid);
            for r in i.resources.iter().filter_map(|r| model.scene.resources.get(*r)) {
                ui.label(r.name.chars().take(120).collect::<String>());
            }
        } else if let Some(id) = self.state.selected_resource
            && let Some(r) = model.scene.resources.get(id)
        {
            ui.label(r.name.chars().take(120).collect::<String>());
        }
        let selected = self.state.selected_instance.is_some() || self.state.selected_resource.is_some();
        ui.add_enabled_ui(selected, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Fit selection").clicked()
                    && let Some(b) = self.selection_bounds()
                {
                    self.state.camera.fit(b);
                    self.changed();
                }
                if ui.button("Hide").clicked() {
                    if let Some(id) = self.state.selected_instance {
                        self.state.visibility.hidden_instances.insert(id);
                    }
                    if let Some(id) = self.state.selected_resource {
                        let set = self.resource_set(id);
                        self.state.visibility.hidden_resources.extend(set);
                    }
                    self.changed();
                }
            });
            if ui.button("Isolate selection").clicked() {
                self.state.visibility = Visibility::default();
                if let Some(id) = self.state.selected_instance {
                    self.state.visibility.isolate_instances.insert(id);
                }
                if let Some(id) = self.state.selected_resource {
                    self.state.visibility.isolate_resources = self.resource_set(id);
                }
                self.changed();
            }
        });
        if ui.button("Show all").clicked() {
            self.state.visibility = Visibility::default();
            self.changed();
        }
        ui.separator();
        ui.small("Placeholder shading. Source units, axes, materials and visual parity are unverified.");
        ui.small("4D schedules and 5D costs are not evaluated in this viewport.");
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.jobs.set_repaint(ui.ctx());
        self.poll();
        if self.initial_open {
            self.initial_open = false;
            self.open();
        }
        ui.horizontal(|ui| {
            ui.heading("Scene viewer");
            ui.label("Experimental");
            if ui.button("Fit all").clicked()
                && let Some(model) = &self.model
            {
                self.state.camera.fit(model.scene.bounds);
                self.changed();
            }
            if ui.checkbox(&mut self.state.camera.y_up, "Y up (manual)").changed() {
                self.changed();
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width((ui.available_width() - 80.).max(100.)).hint_text("Absolute path to scene.json"));
            if ui.add_enabled(!self.jobs.busy(), egui::Button::new("Open")).clicked() {
                self.open();
            }
        });
        ui.horizontal(|ui| {
            ui.set_min_height(26.);
            ui.label(&self.status);
            if self.jobs.busy() {
                ui.spinner();
                if ui.button("Cancel").clicked() {
                    for j in self.jobs.running() {
                        j.cancel.cancel();
                    }
                }
            }
        });
        ui.separator();
        let tokens = Tokens::get(ui.ctx());
        egui::Panel::left("scene_resources").resizable(true).default_size(300.).show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.small("Drag: orbit · Shift/right drag: pan · Wheel: zoom · Click: select surface");
            let size = ui.available_size().max(egui::vec2(1., 1.));
            let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
            ui.painter().rect_filled(rect, 0., tokens.canvas);
            if let Some(texture) = self.texture {
                ui.painter().image(texture, rect, egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)), egui::Color32::WHITE);
            }
            let aspect = f64::from(rect.width()) / f64::from(rect.height().max(1.));
            let factor = (1600. / rect.width().max(rect.height())).min(1.);
            let desired = [(rect.width() * factor).round().max(1.) as u32, (rect.height() * factor).round().max(1.) as u32];
            if desired != self.size {
                self.size = desired;
                self.changed();
            }
            if response.dragged() {
                let delta = ui.input(|i| i.pointer.delta());
                if ui.input(|i| i.modifiers.shift || i.pointer.secondary_down()) {
                    self.state.camera.pan([delta.x, delta.y], rect.height());
                } else {
                    self.state.camera.orbit([delta.x, delta.y]);
                }
                self.changed();
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0. {
                    self.state.camera.zoom(scroll);
                    self.changed();
                }
            }
            if response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                self.pick([f64::from((p.x - rect.left()) / rect.width() * 2. - 1.), f64::from(1. - (p.y - rect.top()) / rect.height() * 2.)], aspect);
            }
        });
        if self.dirty
            && !self.jobs.busy()
            && let (Some(mut gpu), Some(model)) = (self.gpu.take(), self.model.clone())
        {
            let camera = self.state.camera.clone();
            let visibility = self.state.visibility.clone();
            let selected = self.state.selected_instance;
            let selected_resource = self.state.selected_resource;
            let size = self.size;
            self.dirty = false;
            self.jobs.spawn("Render scene", true, move |j| {
                gpu.render(&model.scene, &gpu::FrameData { camera, visibility, selected, selected_resource, size, tokens }, &j.cancel)?;
                Ok(Done::Rendered(gpu))
            });
        }
    }
}
impl eframe::App for SceneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
    fn on_exit(&mut self) {
        for j in self.jobs.running() {
            j.cancel.cancel();
        }
    }
}
fn resource_rows(resources: &[craft_anything_formats::scene_bundle::Resource]) -> Vec<(usize, usize)> {
    let mut children = BTreeMap::<Option<&str>, Vec<usize>>::new();
    for (id, r) in resources.iter().enumerate() {
        children.entry(r.parent_guid.as_deref()).or_default().push(id);
    }
    let mut stack = children.get(&None).into_iter().flatten().rev().map(|id| (*id, 0)).collect::<Vec<_>>();
    let mut rows = Vec::new();
    while let Some((id, depth)) = stack.pop() {
        rows.push((id, depth));
        if let Some(r) = resources.get(id)
            && let Some(next) = children.get(&Some(r.guid.as_str()))
        {
            stack.extend(next.iter().rev().map(|child| (*child, depth + 1)));
        }
        if rows.len() >= resources.len() {
            break;
        }
    }
    rows
}
