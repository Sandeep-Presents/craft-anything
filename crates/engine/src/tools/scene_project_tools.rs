//! Retained project schedules and costs share the scene source identity.
use super::{native_common::spec, scene_tools::LoadedScene, static_input::builder};
use crate::{
    Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset,
    tool::{parse_params, schema_of},
};
pub use craft_anything_analysis::project_model::ProjectModel;
pub use craft_anything_analysis::project_model::{AppearancePolicy, DateMode, Frame};
pub use craft_anything_analysis::project_simulation::{Phase, PhaseState};
use craft_anything_evidence::{Confidence, Coverage, CoverageStatus, Limitation, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct CachedProject {
    pub scene_id: String,
    pub model: Arc<ProjectModel>,
}
pub struct LatestProjectFrame {
    pub scene_id: String,
    pub frame: Arc<Frame>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_size")]
    limit: usize,
}
fn page_size() -> usize {
    100
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum DatePolicy {
    Planned,
    #[default]
    Best,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Evaluate {
    #[serde(default)]
    path: Option<String>,
    focus_wallclock: i64,
    #[serde(default)]
    date_mode: DatePolicy,
    #[serde(default)]
    interval_preview: bool,
    #[serde(default)]
    offset: usize,
    #[serde(default = "page_size")]
    limit: usize,
}
pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "formats_project_tasks_list",
            "List project tasks",
            "Page retained source tasks and calendar-derived schedule bounds. Raw wallclock values retain their displayed-clock meaning; no timezone conversion or dependency rescheduling.",
            "Inspect the project schedule.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<Page>,
            formats_project_tasks_list,
        ),
        spec(
            "formats_project_costs_list",
            "List project cost inputs",
            "Page source resource rates, units and task fixed costs with explicit unresolved fields. Source units are separate from reconstructed mesh quantities.",
            "Inspect budget and cost inputs.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<Page>,
            formats_project_costs_list,
        ),
        spec(
            "formats_project_frame_evaluate",
            "Evaluate a project time",
            "Evaluate supported task appearance and declared source cost semantics at one displayed wallclock time. Retain unresolved profiles, dates and cost allocations instead of inventing values.",
            "Move through the project timeline and inspect its cost state.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<Evaluate>,
            formats_project_frame_evaluate,
        ),
    ]
}
fn model(ctx: &mut Ctx<'_>, path: Option<&str>) -> Result<(Arc<LoadedScene>, Arc<ProjectModel>), ToolError> {
    if let Some(path) = path {
        super::scene_tools::formats_project_open(ctx, json!({"path":path}))?;
    }
    let source = ctx.session.ext.get::<LoadedScene>().ok_or_else(|| ToolError::invalid("Open a source project with formats_project_open first."))?;
    if let Some(cached) = ctx.session.ext.get::<CachedProject>()
        && cached.scene_id == source.scene.id
    {
        return Ok((source, cached.model.clone()));
    }
    let parsed = ProjectModel::from_metadata_with_cancel(&source.metadata, || ctx.cancel.is_cancelled());
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::cancelled());
    }
    let parsed = parsed.map_err(ToolError::invalid)?;
    let parsed = Arc::new(parsed);
    ctx.session.ext.insert(Arc::new(CachedProject { scene_id: source.scene.id.clone(), model: parsed.clone() }));
    Ok((source, parsed))
}
fn formats_project_tasks_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: Page = parse_params(params.clone())?;
    let (source, model) = model(ctx, p.path.as_deref())?;
    let result = serde_json::to_value(model.page_tasks(p.offset, p.limit.clamp(1, 1000))).map_err(|_| ToolError::invalid("Cannot serialize task page."))?;
    let ev = builder(ctx, "formats_project_tasks_list", source.subject.clone(), params)
        .summary("Source project task page")
        .confidence(Confidence::Derived)
        .coverage(Coverage { status: CoverageStatus::Partial, ..Default::default() })
        .result(json!({"scene_id":source.scene.id,"page":result}))
        .target_content(true)
        .build()?;
    Ok(ToolOutput::evidence(ev))
}
fn formats_project_costs_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: Page = parse_params(params.clone())?;
    let (source, model) = model(ctx, p.path.as_deref())?;
    let result = serde_json::to_value(model.page_resources(p.offset, p.limit.clamp(1, 1000))).map_err(|_| ToolError::invalid("Cannot serialize cost page."))?;
    let ev = builder(ctx, "formats_project_costs_list", source.subject.clone(), params)
        .summary("Source project cost inputs")
        .confidence(Confidence::Derived)
        .coverage(Coverage { status: CoverageStatus::Partial, ..Default::default() })
        .result(json!({"scene_id":source.scene.id,"page":result}))
        .target_content(true)
        .build()?;
    Ok(ToolOutput::evidence(ev))
}
fn formats_project_frame_evaluate(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: Evaluate = parse_params(params.clone())?;
    let (source, model) = model(ctx, p.path.as_deref())?;
    let policy = match p.date_mode {
        DatePolicy::Planned => DateMode::Planned,
        DatePolicy::Best => DateMode::Best,
    };
    let appearance = if p.interval_preview { AppearancePolicy::IntervalPreview } else { AppearancePolicy::Strict };
    let frame = Arc::new(model.evaluate_with_policy(p.focus_wallclock, policy, appearance, || ctx.cancel.is_cancelled()).map_err(ToolError::invalid)?);
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::cancelled());
    }
    let limit = p.limit.clamp(1, 1000);
    let mut page = frame.as_ref().clone();
    let phase_total = page.resource_phases.len();
    let cost_total = page.costs.lines.len();
    let reasons_total = page.unresolved_reasons.len();
    page.resource_phases = page.resource_phases.into_iter().skip(p.offset).take(limit).collect();
    page.costs.lines = page.costs.lines.into_iter().skip(p.offset).take(limit).collect();
    page.unresolved_reasons = page.unresolved_reasons.into_iter().skip(p.offset).take(limit).collect();
    let result = json!({"scene_id":source.scene.id,"frame":page,"page":{"offset":p.offset,"limit":limit,"resource_phases_total":phase_total,"cost_lines_total":cost_total,"unresolved_reasons_total":reasons_total}});
    ctx.session.ext.insert(Arc::new(LatestProjectFrame { scene_id: source.scene.id.clone(), frame }));
    let ev=builder(ctx,"formats_project_frame_evaluate",source.subject.clone(),params).summary("Evaluated project appearance and cost state")
        .confidence(Confidence::Derived).coverage(Coverage {status:CoverageStatus::Partial,..Default::default()}).result(result)
        .limitation(Limitation::new("scoped_source_semantics","Unresolved profiles or monetary allocations remain explicit. Source-unit, calendar and phase support is limited to admitted structures; this is not application-wide parity."))
        .target_content(true).build()?;
    Ok(ToolOutput::evidence(ev))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Caller, Engine, Session, Settings};
    #[test]
    fn project_frame_is_paged_but_typed_cache_retains_all_resources() {
        let dir = tempfile::tempdir().unwrap();
        for ((name, _), bytes) in craft_anything_formats::scene_bundle::MEMBERS.iter().zip(craft_anything_testkit::scene_bundle::fixture()) {
            std::fs::write(dir.path().join(name), bytes).unwrap();
        }
        let engine = Engine::new(Session::new(Settings { deep_mode: true, ..Default::default() }));
        engine.call("formats_scene_bundle_open", json!({"path":dir.path().join("scene.json")}), CallOptions::new(Caller::Cli)).unwrap();
        {
            let mut session = engine.session();
            let source = session.ext.get::<LoadedScene>().unwrap();
            let resources = (0..300).map(|i| json!({"guid":format!("resource-{i}"),"name":format!("Resource {i}")})).collect::<Vec<_>>();
            session.ext.insert(Arc::new(LoadedScene {
                scene: source.scene.clone(),
                manifest: source.manifest.clone(),
                subject: source.subject.clone(),
                metadata: Arc::new(json!({"schema_version":4,"tasks":[],"resources":resources,"definition_storage":{}})),
            }));
        }
        let result = engine
            .call("formats_project_frame_evaluate", json!({"focus_wallclock":0,"offset":20,"limit":10}), CallOptions::new(Caller::Cli))
            .unwrap()
            .to_json();
        assert_eq!(result["result"]["page"]["resource_phases_total"], 300);
        assert_eq!(result["result"]["frame"]["resource_phases"].as_array().unwrap().len(), 10);
        assert_eq!(engine.session().ext.get::<LatestProjectFrame>().unwrap().frame.resource_phases.len(), 300);
    }
    #[test]
    fn project_tools_require_deep_mode_and_open_source() {
        let engine = Engine::new(Session::new(Settings::default()));
        for id in ["formats_project_open", "formats_project_tasks_list", "formats_project_costs_list", "formats_project_frame_evaluate"] {
            assert!(engine.call(id, json!({}), CallOptions::new(Caller::Cli)).is_err());
        }
        let engine = Engine::new(Session::new(Settings { deep_mode: true, ..Default::default() }));
        assert!(engine.call("formats_project_tasks_list", json!({}), CallOptions::new(Caller::Cli)).unwrap_err().message.contains("formats_project_open"));
    }
}
