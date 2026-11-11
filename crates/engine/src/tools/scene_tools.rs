//! Read-only scene bundles: all loading and surface selection work without the UI.
use super::native_common::spec;
use super::static_input::{builder, format_err, load_path};
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_evidence::{Confidence, Coverage, CoverageStatus, Limitation, Subject, ToolError};
use craft_anything_formats::scene_bundle::{self, Scene, Visibility};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An immutable loaded model; session extensions hand cheap snapshots to clients.
pub struct LoadedScene {
    pub scene: Arc<Scene>,
    pub manifest: PathBuf,
    pub subject: Subject,
    pub metadata: Arc<Value>,
}
pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "formats_project_open",
            "Open a project model",
            "Read the admitted 6.5.5.28 project layout directly into the scene cache, including framed Tasks, Resources and geometry. No source application or pre-generated bundle is required. Unsupported layouts fail explicitly.",
            "Open a project file for 3D, schedule and cost inspection.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<OpenParams>,
            formats_project_open,
        ),
        spec(
            "formats_scene_bundle_open",
            "Open a scene bundle",
            "Validate and retain an original version-1 scene bundle from an absolute scene.json path. Checks fixed member names, ranges, geometry indices, graph transforms and Resource membership. Geometry bytes stay in a typed session cache. Does not launch a viewer or infer units, axes or materials.",
            "Read a saved 3D scene for inspection and viewing.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<OpenParams>,
            formats_scene_bundle_open,
        ),
        spec(
            "formats_scene_resources_list",
            "List scene resources",
            "Page the loaded scene's Resource identities, hierarchy and instance counts. Requires formats_scene_bundle_open first.",
            "See which resources have visible geometry.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<ListParams>,
            formats_scene_resources_list,
        ),
        spec(
            "formats_scene_surface_pick",
            "Pick a scene surface",
            "Find the nearest triangle hit by a ray in stored world coordinates, optionally filtering Resources. Checks at most two million candidate triangles and reports incomplete instead of claiming a hit when a budget or singular transform prevents certainty. Distances retain unverified native units.",
            "Select an object using a viewing ray.",
            Toolset::Formats,
            Effects::EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Nothing,
            schema_of::<PickParams>,
            formats_scene_surface_pick,
        ),
    ]
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    path: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    200
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PickParams {
    scene_id: String,
    #[serde(default)]
    hidden_instances: Vec<String>,
    #[serde(default)]
    isolate_instances: Vec<String>,
    origin: [f64; 3],
    direction: [f64; 3],
    #[serde(default)]
    hidden_resources: Vec<String>,
    #[serde(default)]
    isolate_resources: Vec<String>,
}
fn scene(ctx: &Ctx<'_>) -> Result<Arc<LoadedScene>, ToolError> {
    ctx.session.ext.get::<LoadedScene>().ok_or_else(|| ToolError::invalid("Open a scene bundle with formats_scene_bundle_open first."))
}
fn member(root: &Path, name: &str, cap: usize, total: &mut usize) -> Result<Vec<u8>, ToolError> {
    let path = root.join(name);
    let actual = path.canonicalize().map_err(|e| ToolError::invalid(format!("Cannot resolve scene member {name}: {e}")))?;
    if actual.parent() != Some(root) {
        return Err(ToolError::invalid("Scene members must be regular files inside the bundle folder."));
    }
    let meta = actual.metadata().map_err(|e| ToolError::invalid(format!("Cannot inspect scene member: {e}")))?;
    let len = usize::try_from(meta.len()).map_err(|_| ToolError::invalid("Scene member is too large."))?;
    *total = total.checked_add(len).ok_or_else(|| ToolError::invalid("Scene input size overflows."))?;
    if !meta.is_file() || len > cap || *total > scene_bundle::MAX_TOTAL_BYTES {
        return Err(ToolError::invalid("Scene member or total input exceeds the bounded reader limits."));
    }
    craft_anything_platform::fs::read_file_capped(&actual, cap as u64)
}
pub(crate) fn formats_project_open(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: OpenParams = parse_params(params.clone())?;
    let input = load_path(&p.path, 256 * 1024 * 1024)?;
    let imported = craft_anything_formats::sp_project::parse(&input.bytes, |progress| {
        ctx.report(Some(progress), "Decoding project data and geometry");
        !ctx.cancel.is_cancelled()
    });
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::cancelled());
    }
    let imported = imported.map_err(|e| format_err(e, "an admitted project model"))?;
    let data = Arc::new(imported.scene);
    let result = json!({"scene_id":data.id,"source_sha256":data.source_sha256,"mesh_instances":data.instances.len(),"definitions":data.meshes.len(),"vertices":data.positions.len(),"resources":data.resources.len(),"tasks":imported.metadata.get("tasks").and_then(Value::as_array).map(Vec::len),"object_colours":data.object_colors.len(),"object_colour_policy":scene_bundle::OBJECT_COLOR_POLICY,"native_units_verified":false,"native_axes_verified":false});
    let count = data.instances.len();
    ctx.session.ext.insert(Arc::new(LoadedScene {
        scene: data,
        manifest: input.path.clone(),
        subject: input.subject.clone(),
        metadata: Arc::new(imported.metadata),
    }));
    let ev=builder(ctx,"formats_project_open",input.subject,params).summary(format!("Opened a source project with {count} mesh instances"))
        .confidence(Confidence::Derived).coverage(Coverage::complete(count as u64)).result(result).target_content(true)
        .limitation(Limitation::new("admitted_layout_only","Only the observed serialization layout is supported; engineering units, axes, source materials and application-wide simulation parity require separate evidence." )).build()?;
    Ok(ToolOutput::evidence(ev))
}
fn formats_scene_bundle_open(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: OpenParams = parse_params(params.clone())?;
    let input = load_path(&p.path, 1024 * 1024)?;
    if input.path.file_name().and_then(|n| n.to_str()) != Some("scene.json") {
        return Err(ToolError::invalid("Pass the bundle's scene.json manifest."));
    }
    let manifest = input.path.canonicalize().map_err(|e| ToolError::invalid(format!("Cannot resolve scene manifest: {e}")))?;
    let root = manifest.parent().ok_or_else(|| ToolError::invalid("Scene manifest has no parent folder."))?;
    let mut total = 0usize;
    let mut files = Vec::new();
    for (i, (name, cap)) in scene_bundle::MEMBERS.iter().enumerate() {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::cancelled());
        }
        ctx.report(Some(i as f32 / 50.), format!("Reading scene member {name}"));
        files.push(member(root, name, *cap, &mut total)?);
    }
    let take = |i: usize| files.get(i).map(Vec::as_slice).ok_or_else(|| ToolError::invalid("Scene member inventory is incomplete."));
    if input.bytes.as_ref() != take(0)? {
        return Err(ToolError::invalid("Scene manifest changed while opening; retry with a stable bundle."));
    }
    let parts = scene_bundle::Inputs {
        manifest: take(0)?,
        nodes: take(1)?,
        meshes: take(2)?,
        instances: take(3)?,
        bindings: take(4)?,
        project: take(5)?,
        binary: take(6)?,
    };
    let mut progress = 0.2f32;
    let parsed = scene_bundle::parse(parts, |fraction| {
        let next = 0.2 + fraction * 0.7;
        if next > progress {
            progress = next;
            ctx.report(Some(next), "Checking scene geometry and references");
        }
        !ctx.cancel.is_cancelled()
    });
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::cancelled());
    }
    let data = Arc::new(parsed.map_err(|e| format_err(e, "a version-1 scene bundle"))?);
    let result = json!({"scene_id":data.id,"source_sha256_declared":data.source_sha256,"nodes":data.node_count,"meshes":data.meshes.len(),"mesh_instances":data.instances.len(),"vertices":data.positions.len(),"triangles_in_definitions":data.triangles,"resources":data.resources.len(),"bounds":data.bounds,"coordinate_unit_declared":data.unit_declaration,"axes_declared":data.axis_declaration,"units_verified":false,"axes_verified":false,"materials_verified":false});
    let count = data.instances.len() as u64;
    ctx.session.ext.insert(Arc::new(LoadedScene {
        scene: data,
        manifest,
        subject: input.subject.clone(),
        metadata: Arc::new(serde_json::from_slice(take(5)?).map_err(|_| ToolError::invalid("Project metadata is invalid JSON"))?),
    }));
    ctx.report(Some(1.), "Scene ready");
    let ev = builder(ctx, "formats_scene_bundle_open", input.subject, params)
        .summary(format!("Loaded {} mesh instances from a validated scene bundle", count))
        .confidence(Confidence::Observed)
        .coverage(Coverage::complete(count))
        .result(result)
        .target_content(true)
        .limitation(Limitation::new(
            "unverified_render_semantics",
            "Scene geometry and graph were checked; units, axes, materials and native visual parity remain unverified.",
        ))
        .build()?;
    Ok(ToolOutput::evidence(ev))
}
fn formats_scene_resources_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ListParams = parse_params(params.clone())?;
    let loaded = scene(ctx)?;
    let limit = p.limit.clamp(1, 1000);
    let rows = loaded
        .scene
        .resources
        .iter()
        .enumerate()
        .skip(p.offset)
        .take(limit)
        .map(|(id, r)| {
            json!({"index":id,"guid":r.guid,"name":r.name,"parent_guid":r.parent_guid,
            "mesh_instances":loaded.scene.resource_instance_counts.get(id).copied().unwrap_or(0)})
        })
        .collect::<Vec<_>>();
    let total = loaded.scene.resources.len();
    let end = p.offset.saturating_add(rows.len());
    let next = (end < total).then_some(end as u64);
    let coverage = Coverage {
        status: if p.offset == 0 && rows.len() == total { CoverageStatus::Complete } else { CoverageStatus::Partial },
        examined: Some(rows.len() as u64),
        total: Some(total as u64),
        next_offset: next,
    };
    let ev = builder(ctx, "formats_scene_resources_list", loaded.subject.clone(), params)
        .summary(format!("{} of {total} scene Resources", rows.len()))
        .confidence(Confidence::Observed)
        .coverage(coverage)
        .result(json!({"scene_id":loaded.scene.id,"offset":p.offset,"total":total,"next_offset":next,"resources":rows}))
        .target_content(true)
        .build()?;
    Ok(ToolOutput::evidence(ev))
}
fn filter(scene: &Scene, ids: &[String]) -> Result<BTreeSet<usize>, ToolError> {
    if ids.len() > 10_000 {
        return Err(ToolError::invalid("Too many scene Resource filters."));
    }
    let known = scene.resources.iter().enumerate().map(|(id, r)| (r.guid.as_str(), id)).collect::<std::collections::BTreeMap<_, _>>();
    ids.iter().map(|id| known.get(id.as_str()).copied().ok_or_else(|| ToolError::invalid("A Resource filter does not belong to the loaded scene."))).collect()
}
fn formats_scene_surface_pick(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: PickParams = parse_params(params.clone())?;
    let loaded = scene(ctx)?;
    if p.scene_id != loaded.scene.id {
        return Err(ToolError::invalid("The selection belongs to a different scene."));
    }
    let hidden = filter(&loaded.scene, &p.hidden_resources)?;
    let isolate = filter(&loaded.scene, &p.isolate_resources)?;
    let instance_filter = |ids: &[String]| -> Result<BTreeSet<usize>, ToolError> {
        if ids.len() > 100_000 {
            return Err(ToolError::invalid("Too many instance filters."));
        }
        let wanted = ids.iter().collect::<BTreeSet<_>>();
        let found = loaded.scene.instances.iter().enumerate().filter(|(_, i)| wanted.contains(&i.id)).map(|(id, _)| id).collect::<BTreeSet<_>>();
        if wanted.len() != found.len() {
            return Err(ToolError::invalid("Unknown scene instance filter."));
        }
        Ok(found)
    };
    let filters = Visibility {
        hidden_resources: hidden,
        isolate_resources: isolate,
        hidden_instances: instance_filter(&p.hidden_instances)?,
        isolate_instances: instance_filter(&p.isolate_instances)?,
    };
    let hit = loaded.scene.pick(p.origin, p.direction, &filters, || !ctx.cancel.is_cancelled()).map_err(|e| format_err(e, "a scene selection ray"))?;
    if ctx.cancel.is_cancelled() {
        return Err(ToolError::cancelled());
    }
    let selected=hit.instance.and_then(|i|loaded.scene.instances.get(i)).map(|i|json!({"instance_id":i.id,"entity_guid":i.entity_guid,"resource_guids":i.resources.iter().filter_map(|r|loaded.scene.resources.get(*r)).map(|r|r.guid.as_str()).collect::<Vec<_>>()}));
    let subject = loaded.subject.clone();
    let ev = builder(ctx, "formats_scene_surface_pick", subject, params)
        .summary(if hit.complete { "Scene surface selection completed" } else { "Scene surface selection is incomplete" })
        .confidence(Confidence::Derived)
        .coverage(Coverage {
            status: if hit.complete { CoverageStatus::Complete } else { CoverageStatus::Partial },
            examined: Some(hit.triangles_tested as u64),
            total: None,
            next_offset: None,
        })
        .result(json!({"scene_id":loaded.scene.id,"pick":hit,"selected":selected,"distance_unit":"unverified native world units"}))
        .build()?;
    Ok(ToolOutput::evidence(ev))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Caller, Engine, Session, Settings};
    use scene_bundle::{
        Instance, Mesh, Resource,
        math::{Bounds, IDENTITY},
    };
    fn engine() -> Engine {
        let mut session = Session::new(Settings { deep_mode: true, ..Default::default() });
        let target = crate::Target::from_bytes(PathBuf::from("scene.json"), "scene.json".into(), b"synthetic-manifest".to_vec());
        let bounds = Bounds([[-1., -1., 0.], [1., 1., 0.]]);
        let scene = Scene {
            id: "synthetic-scene".into(),
            source_sha256: "a".repeat(64),
            unit_declaration: "unknown".into(),
            axis_declaration: "unknown".into(),
            resources: vec![
                Resource { guid: "resource-a".into(), name: "Synthetic resource".into(), parent_guid: None },
                Resource { guid: "resource-b".into(), name: "Empty resource".into(), parent_guid: None },
            ],
            resource_instance_counts: vec![1, 0],
            meshes: vec![Mesh { vertex_start: 0, index_start: 0, vertex_count: 3, triangle_count: 1, origin: [0.; 3], bounds }],
            instances: vec![Instance {
                id: "instance-a".into(),
                entity_guid: "entity-a".into(),
                mesh: 0,
                matrix: IDENTITY,
                inverse: Some(IDENTITY),
                bounds,
                resources: vec![0],
            }],
            positions: vec![[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]],
            gpu_positions: Vec::new(),
            gpu_indices: [0u32, 1, 2].into_iter().flat_map(u32::to_le_bytes).collect(),
            bounds,
            render_origin: [0.; 3],
            triangles: 1,
            node_count: 1,
            source_colors: Default::default(),
            object_colors: Default::default(),
        };
        session.ext.insert(Arc::new(LoadedScene {
            scene: Arc::new(scene),
            manifest: PathBuf::from("scene.json"),
            subject: target.subject(),
            metadata: Arc::new(json!({})),
        }));
        Engine::new(session)
    }
    #[test]
    fn pages_preserve_provenance_and_disclose_partial_coverage() {
        let e = engine();
        let a = e.call("formats_scene_resources_list", json!({"limit":1}), CallOptions::new(Caller::Cli)).unwrap().to_json();
        let b = e.call("formats_scene_resources_list", json!({"offset":1,"limit":1}), CallOptions::new(Caller::Cli)).unwrap().to_json();
        assert_eq!(a["coverage"]["status"], "partial");
        assert_eq!(b["coverage"]["status"], "partial");
        assert_eq!(a["subject"], b["subject"]);
        let expected = e.session().ext.get::<LoadedScene>().unwrap().subject.clone();
        assert_eq!(a["subject"], serde_json::to_value(expected).unwrap());
        assert_eq!(a["result"]["resources"][0]["mesh_instances"], 1);
    }
    #[test]
    fn stale_scene_and_unknown_filters_are_rejected_and_hidden_surfaces_are_not_picked() {
        let e = engine();
        let params = json!({"scene_id":"synthetic-scene","origin":[0.,0.,3.],"direction":[0.,0.,-1.]});
        let hit = e.call("formats_scene_surface_pick", params.clone(), CallOptions::new(Caller::Cli)).unwrap().to_json();
        assert_eq!(hit["result"]["pick"]["instance"], 0);
        let mut p = params.clone();
        p["scene_id"] = json!("stale");
        assert!(e.call("formats_scene_surface_pick", p, CallOptions::new(Caller::Cli)).is_err());
        let mut p = params.clone();
        p["hidden_instances"] = json!(["unknown"]);
        assert!(e.call("formats_scene_surface_pick", p, CallOptions::new(Caller::Cli)).is_err());
        let mut p = params;
        p["hidden_instances"] = json!(["instance-a"]);
        let hidden = e.call("formats_scene_surface_pick", p, CallOptions::new(Caller::Cli)).unwrap().to_json();
        assert!(hidden["result"]["pick"]["instance"].is_null());
    }
    #[test]
    fn open_requires_absolute_manifest_and_headless_tools_never_open_dialogs() {
        let e = engine();
        assert!(e.call("formats_scene_bundle_open", json!({"path":"scene.json"}), CallOptions::new(Caller::Cli)).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("other.json"), b"{}").unwrap();
        assert!(e.call("formats_scene_bundle_open", json!({"path":dir.path().join("other.json")}), CallOptions::new(Caller::Cli)).is_err());
        std::fs::write(dir.path().join("scene.json"), b"{}").unwrap();
        assert!(e.call("formats_scene_bundle_open", json!({"path":dir.path().join("scene.json")}), CallOptions::new(Caller::Cli)).is_err());
    }
}
