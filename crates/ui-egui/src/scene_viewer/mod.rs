//! Experimental scene-bundle client. Parsing and picking dispatch through the shared engine.
pub mod camera;
pub mod draw_plan;
mod gpu;
pub mod project_panel;
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
    pub project: project_panel::State,
}
enum Done {
    Evaluated {
        key: (String, i64, bool, bool),
        latest: Arc<craft_anything_engine::tools::scene_project_tools::LatestProjectFrame>,
        project: Arc<craft_anything_engine::tools::scene_project_tools::CachedProject>,
        hidden: BTreeSet<usize>,
        phases: BTreeMap<usize, u8>,
    },
    Chosen(Option<String>),
    Loaded(Arc<LoadedScene>, Box<gpu::Gpu>),
    Rendered(Box<gpu::Gpu>, Result<(), String>),
    Picked {
        scene_id: String,
        revision: u64,
        selected: Option<usize>,
        complete: bool,
    },
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
    frame_key: Option<(String, i64, bool, bool)>,
    pub project_frame: Option<Arc<craft_anything_engine::tools::scene_project_tools::LatestProjectFrame>>,
    project_model: Option<Arc<craft_anything_engine::tools::scene_project_tools::CachedProject>>,
    timeline_hidden: BTreeSet<usize>,
    timeline_phases: BTreeMap<usize, u8>,
    last_play_tick: std::time::Instant,
    render_error: Option<String>,
    last_draw: Option<(draw_plan::DrawStats, u64)>,
    evaluation_failed: bool,
}
impl SceneApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: String) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, ThemeKind::Studio, false);
        Self::with_engine(Arc::new(Engine::new(Session::new(Settings { deep_mode: true, ..Default::default() }))), cc.wgpu_render_state.clone(), path)
    }
    /// Embedded client shares the application's engine and its Deep-mode settings.
    pub fn with_engine(engine: Arc<Engine>, graphics: Option<RenderState>, path: String) -> Self {
        Self {
            state: ViewState::default(),
            initial_open: !path.is_empty(),
            path,
            status: "Open a .sp project or scene.json bundle to start.".into(),
            model: None,
            engine,
            graphics,
            jobs: Jobs::default(),
            gpu: None,
            texture: None,
            rows: Vec::new(),
            dirty: false,
            revision: 0,
            size: [1000, 700],
            frame_key: None,
            project_frame: None,
            project_model: None,
            timeline_hidden: BTreeSet::new(),
            timeline_phases: BTreeMap::new(),
            last_play_tick: std::time::Instant::now(),
            render_error: None,
            last_draw: None,
            evaluation_failed: false,
        }
    }
    pub fn request_open(&mut self, path: String) {
        for job in self.jobs.running() {
            job.cancel.cancel();
        }
        self.path = path;
        self.initial_open = true;
    }
    pub fn ready(&self) -> bool {
        self.texture.is_some() && self.gpu.is_some() && !self.jobs.busy() && !self.dirty
    }
    pub fn draw_stats(&self) -> Option<(&draw_plan::DrawStats, u64)> {
        self.last_draw.as_ref().map(|(stats, ms)| (stats, *ms))
    }
    /// The last worker error, for verification clients that must fail promptly.
    pub fn error(&self) -> Option<&str> {
        if self.dirty || self.jobs.busy() {
            return None;
        }
        if let Some(e) = &self.render_error {
            return Some(e);
        }
        self.jobs.list.iter().rev().find_map(|j| j.outcome.as_ref()).and_then(|v| v.as_ref().err()).map(String::as_str)
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
            let tool = if path.to_ascii_lowercase().ends_with(".sp") { "formats_project_open" } else { "formats_scene_bundle_open" };
            engine
                .call(tool, json!({"path":path}), CallOptions::new(Caller::Ui).with_cancel(j.cancel.clone()).with_progress(&report))
                .map_err(|e| e.for_people())?;
            let model = engine.session().ext.get::<LoadedScene>().ok_or("The loaded scene is unavailable")?;
            let gpu = gpu::Gpu::new(&graphics, &model.scene, &j.cancel)?;
            Ok(Done::Loaded(model, Box::new(gpu)))
        });
    }
    fn poll(&mut self) {
        for done in self.jobs.poll() {
            match done.result {
                Ok(Done::Chosen(Some(path))) => {
                    self.path = path;
                    self.open();
                }
                Ok(Done::Chosen(None)) => {}
                Ok(Done::Evaluated { key, latest, project, hidden, phases }) => {
                    if self.evaluation_key().as_ref() == Some(&key) {
                        self.frame_key = Some(key);
                        self.project_frame = Some(latest);
                        self.project_model = Some(project);
                        self.timeline_hidden = hidden;
                        self.timeline_phases = phases;
                        self.changed();
                    }
                }
                Ok(Done::Loaded(model, gpu)) => {
                    self.last_draw = None;
                    if let (Some(graphics), Some(old)) = (&self.graphics, self.texture.take()) {
                        graphics.renderer.write().free_texture(&old);
                    }
                    if self.render_error.take().is_some() {
                        self.status = "Scene rendering recovered.".into();
                    }
                    self.state = ViewState::default();
                    self.state.project.interval_preview = true;
                    self.state.project.object_colors = !model.scene.object_colors.is_empty();
                    self.frame_key = None;
                    self.project_frame = None;
                    self.project_model = None;
                    self.timeline_hidden.clear();
                    self.timeline_phases.clear();
                    self.state.project.focus = model
                        .metadata
                        .get("tasks")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t.get("planned_start_wallclock_scalar").and_then(|v| v.as_i64()))
                        .filter(|t| *t != 32535244799)
                        .min()
                        .unwrap_or(0);
                    self.state.camera.fit(model.scene.bounds);
                    self.rows = resource_rows(&model.scene.resources);
                    self.status = format!("{} mesh instances · {} resources", model.scene.instances.len(), model.scene.resources.len());
                    self.model = Some(model);
                    self.gpu = Some(gpu);
                    self.changed();
                }
                Ok(Done::Rendered(gpu, result)) => {
                    if let Err(error) = result {
                        self.render_error = Some(error.clone());
                        self.status = error;
                        self.gpu = Some(gpu);
                        continue;
                    }
                    if self.render_error.take().is_some() {
                        self.status = "Scene rendering recovered.".into();
                    }
                    self.last_draw = gpu.stats.clone().map(|stats| (stats, gpu.elapsed_ms));
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
                    if self.jobs.list.iter().any(|j| j.id == done.job && j.title == "Evaluate project time") {
                        self.evaluation_failed = true;
                        self.project_frame = None;
                        self.timeline_hidden.clear();
                        self.timeline_phases.clear();
                        self.state.project.playing = false;
                        self.changed();
                    }
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
        let effective = self.effective_visibility();
        let f = &effective;
        let resources = |ids: &BTreeSet<usize>| ids.iter().filter_map(|id| model.scene.resources.get(*id)).map(|r| r.guid.clone()).collect::<Vec<_>>();
        let instances = |ids: &BTreeSet<usize>| ids.iter().filter_map(|id| model.scene.instances.get(*id)).map(|i| i.id.clone()).collect::<Vec<_>>();
        let params = json!({"scene_id":model.scene.id,"origin":origin,"direction":direction,"hidden_resources":resources(&f.hidden_resources),"isolate_resources":resources(&f.isolate_resources),"hidden_instances":instances(&f.hidden_instances),"isolate_instances":instances(&f.isolate_instances)});
        let engine = self.engine.clone();
        let revision = self.revision;
        self.jobs.spawn("Select surface", true, move |j| {
            let result = engine
                .call("formats_scene_surface_pick", params, CallOptions::new(Caller::Ui).with_cancel(j.cancel.clone()))
                .map_err(|e| e.for_people())?
                .to_json();
            let result = result.get("result").ok_or("Selection result is absent")?;
            let hit = result.get("pick").ok_or("Selection details are absent")?;
            let selected = hit.get("instance").and_then(|v| v.as_u64()).and_then(|v| usize::try_from(v).ok());
            Ok(Done::Picked { scene_id: model.scene.id.clone(), revision, selected, complete: hit.get("complete").and_then(|v| v.as_bool()).unwrap_or(false) })
        });
    }
    fn evaluation_key(&self) -> Option<(String, i64, bool, bool)> {
        Some((self.model.as_ref()?.scene.id.clone(), self.state.project.focus, self.state.project.best_dates, self.state.project.interval_preview))
    }
    fn effective_visibility(&self) -> Visibility {
        let mut out = self.state.visibility.clone();
        if self.state.project.mode != project_panel::Mode::Model {
            out.hidden_instances.extend(&self.timeline_hidden);
        }
        out
    }
    fn evaluate(&mut self) {
        if self.jobs.busy() {
            return;
        }
        let Some(key) = self.evaluation_key() else {
            return;
        };
        let Some(model) = self.model.clone() else {
            return;
        };
        self.frame_key = Some(key.clone());
        self.evaluation_failed = false;
        self.project_frame = None;
        self.timeline_hidden.clear();
        self.timeline_phases.clear();
        let engine = self.engine.clone();
        self.jobs.spawn("Evaluate project time", true, move |j| {
            engine
                .call(
                    "formats_project_frame_evaluate",
                    json!({"focus_wallclock":key.1,"date_mode":if key.2 {"best"}else{"planned"},"interval_preview":key.3}),
                    CallOptions::new(Caller::Ui).with_cancel(j.cancel.clone()),
                )
                .map_err(|e| e.message)?;
            let (latest, project) = {
                let session = engine.session();
                (
                    session.ext.get::<craft_anything_engine::tools::scene_project_tools::LatestProjectFrame>(),
                    session.ext.get::<craft_anything_engine::tools::scene_project_tools::CachedProject>(),
                )
            };
            let latest = latest.ok_or("Project frame is unavailable")?;
            let project = project.ok_or("Project model is unavailable")?;
            if latest.scene_id != model.scene.id {
                return Err("Project changed during evaluation; reopen the source.".into());
            }
            let (hidden, phases) = project_panel::phase_state(&model.scene, &latest.frame);
            Ok(Done::Evaluated { key, latest, project, hidden, phases })
        });
    }
    fn select_task(&mut self, guid: &str) {
        let Some(model) = &self.model else {
            return;
        };
        let mut selected = BTreeSet::new();
        if let Some(task) =
            model.metadata.get("tasks").and_then(|v| v.as_array()).into_iter().flatten().find(|t| t.get("guid").and_then(|v| v.as_str()) == Some(guid))
        {
            for id in task.get("assignments").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|a| a.get("resource_guid").and_then(|v| v.as_str())) {
                if let Some(index) = model.scene.resources.iter().position(|r| r.guid == id) {
                    selected.insert(index);
                }
            }
        }
        if !selected.is_empty() {
            self.state.visibility = Visibility { isolate_resources: selected, ..Default::default() };
            self.changed();
        }
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
        let height = ui.available_height();
        egui::ScrollArea::vertical().id_salt("scene_sidebar").show(ui, |ui| self.sidebar_contents(ui, height));
    }
    fn sidebar_contents(&mut self, ui: &mut egui::Ui, panel_height: f32) {
        ui.heading("Resources");
        ui.add(egui::TextEdit::singleline(&mut self.state.search).hint_text("Find a resource"));
        let Some(model) = self.model.clone() else {
            ui.label("No scene loaded");
            return;
        };
        ui.small(format!("Showing: {}", model.manifest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()))
            .on_hover_text(model.manifest.display().to_string());
        ui.label(format!("{} definitions · {} instances", model.scene.meshes.len(), model.scene.instances.len()));
        ui.small(format!("{} explicit object colours", model.scene.object_colors.len()));
        ui.separator();
        let query = self.state.search.to_lowercase();
        let rows = self
            .rows
            .iter()
            .copied()
            .filter(|(id, _)| model.scene.resources.get(*id).is_some_and(|r| query.is_empty() || r.name.to_lowercase().contains(&query)))
            .collect::<Vec<_>>();
        egui::ScrollArea::vertical().id_salt("resource_tree").max_height((panel_height - 320.).clamp(48., 300.)).show_rows(ui, 22., rows.len(), |ui, range| {
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
        ui.small("Experimental shading. Source material precedence and full visual parity remain unverified.");
        if let Some((stats, ms)) = self.draw_stats() {
            ui.small(format!(
                "{} drawn / {} culled / {} hidden; {} triangles; {} ms",
                stats.visible, stats.frustum_culled, stats.hidden, stats.expanded_triangles, ms
            ));
        }
        ui.small("4D/5D interpretation limits remain explicit in the project panels.");
    }
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.jobs.set_repaint(ui.ctx());
        self.poll();
        if self.initial_open && !self.jobs.busy() {
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
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .desired_width((ui.available_width() - 165.).max(80.))
                    .hint_text("Absolute path to .sp or scene.json"),
            );
            if ui.add_enabled(!self.jobs.busy(), egui::Button::new("Browse")).clicked() {
                self.jobs.spawn("Choose project", true, |_| {
                    Ok(Done::Chosen(
                        rfd::FileDialog::new().add_filter("Project or scene", &["sp", "json"]).pick_file().map(|p| p.to_string_lossy().into_owned()),
                    ))
                });
            }
            if ui.add_enabled(!self.jobs.busy(), egui::Button::new("Open")).clicked() {
                self.open();
            }
        });
        ui.horizontal(|ui| {
            ui.set_min_height(26.);
            ui.label(&self.status);
            let busy = self.jobs.busy();
            ui.add_visible(busy, egui::Spinner::new().size(16.));
            if ui.add_enabled(busy, egui::Button::new("Cancel")).clicked() {
                for j in self.jobs.running() {
                    j.cancel.cancel();
                }
            }
        });
        if project_panel::controls(ui, &mut self.state.project, self.project_frame.as_ref().map(|f| f.frame.as_ref())) {
            self.changed();
        }
        if self.evaluation_failed && ui.button("Retry timeline evaluation").clicked() {
            self.frame_key = None;
            self.evaluation_failed = false;
        }
        if self.state.project.playing && self.state.project.mode != project_panel::Mode::Model {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            if !self.jobs.busy() && self.last_play_tick.elapsed() > std::time::Duration::from_millis(100) {
                self.last_play_tick = std::time::Instant::now();
                self.state.project.focus = self.state.project.focus.saturating_add(43200);
                if let Some(end) = self.project_frame.as_ref().and_then(|f| f.frame.timeline.end) {
                    let stop = end.saturating_add(86400);
                    if self.state.project.focus >= stop {
                        self.state.project.focus = stop;
                        self.state.project.playing = false;
                    }
                }
                self.changed();
            }
        }
        ui.separator();
        let tokens = Tokens::get(ui.ctx());
        egui::Panel::left("scene_resources").resizable(true).default_size(300.).show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some((frame, model)) = self.project_frame.as_ref().zip(self.project_model.as_ref())
                && let Some(task) = project_panel::details(ui, &mut self.state.project, &frame.frame, &model.model)
            {
                self.select_task(&task);
            }
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
        if self.state.project.mode != project_panel::Mode::Model && !self.jobs.busy() && self.frame_key != self.evaluation_key() {
            self.evaluate();
        }
        if self.dirty
            && !self.jobs.busy()
            && let (Some(mut gpu), Some(model)) = (self.gpu.take(), self.model.clone())
        {
            let camera = self.state.camera.clone();
            let visibility = self.effective_visibility();
            let phases = if self.state.project.mode == project_panel::Mode::Model { BTreeMap::new() } else { self.timeline_phases.clone() };
            let source_colors = self.state.project.source_colors;
            let object_colors = self.state.project.object_colors;
            let selected = self.state.selected_instance;
            let selected_resource = self.state.selected_resource;
            let size = self.size;
            self.dirty = false;
            self.jobs.spawn("Render scene", true, move |j| {
                let result = gpu.render(
                    &model.scene,
                    &gpu::FrameData { camera, visibility, selected, selected_resource, size, tokens, phases, source_colors, object_colors },
                    &j.cancel,
                );
                Ok(Done::Rendered(gpu, result))
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
