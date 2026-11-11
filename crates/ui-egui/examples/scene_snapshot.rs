//! Local GPU and interaction verification: scene_snapshot <scene.json> <output-directory>.
#![forbid(unsafe_code)]
use craft_anything_ui_egui::scene_viewer::SceneApp;
use egui_kittest::{Harness, kittest::Queryable};
use std::{
    path::Path,
    time::{Duration, Instant},
};
fn settle(h: &mut Harness<SceneApp>) -> Result<(), String> {
    let start = Instant::now();
    let mut quiet = 0;
    loop {
        h.step();
        if let Some(e) = h.state().error() {
            return Err(e.to_owned());
        }
        if h.state().ready() {
            quiet += 1;
        } else {
            quiet = 0;
        }
        if quiet >= 4 {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(60) {
            return Err(format!("Viewer did not become ready: {}", h.state().status));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn snapshot(path: String, output: &Path) -> Result<(), String> {
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.power_preference = eframe::wgpu::PowerPreference::HighPerformance;
    setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters, _| {
        adapters
            .iter()
            .find(|a| a.get_info().device_type == eframe::wgpu::DeviceType::DiscreteGpu)
            .or_else(|| adapters.iter().find(|a| a.get_info().device_type == eframe::wgpu::DeviceType::IntegratedGpu))
            .cloned()
            .ok_or_else(|| "A hardware GPU is required for this large-scene verifier".into())
    }));
    let mut h = Harness::builder().with_size(egui::vec2(1400., 900.)).wgpu_setup(eframe::egui_wgpu::WgpuSetup::CreateNew(setup)).build_eframe(move |cc| {
        if let Some(g) = &cc.wgpu_render_state {
            eprintln!("Graphics adapter: {:?}", g.adapter.get_info());
        }
        SceneApp::new(cc, path)
    });
    settle(&mut h)?;
    h.render()?.save(output.join("scene-fit.png")).map_err(|e| e.to_string())?;
    let model = h.state().model.clone().ok_or("No scene loaded")?;
    let scene = &model.scene;
    // Aim through a real triangle centroid rather than assuming screen centre contains geometry.
    let instance = scene.instances.iter().find(|i| scene.meshes.get(i.mesh).is_some_and(|m| m.triangle_count > 0)).ok_or("No drawable instance")?;
    let mesh = scene.meshes.get(instance.mesh).ok_or("Missing mesh")?;
    let mut local = [0.; 3];
    for corner in 0..3 {
        let at = (mesh.index_start + corner) * 4;
        let index = u32::from_le_bytes(scene.gpu_indices.get(at..at + 4).ok_or("Missing triangle")?.try_into().map_err(|_| "Bad triangle")?) as usize;
        let p = scene.positions.get(mesh.vertex_start + index).ok_or("Missing position")?;
        for (v, c) in local.iter_mut().zip(p) {
            *v += c / 3.;
        }
    }
    h.state_mut().state.camera.target = craft_anything_formats::scene_bundle::math::transform(&instance.matrix, local);
    h.state_mut().changed();
    settle(&mut h)?;
    h.state_mut().pick([0., 0.], 1.5);
    settle(&mut h)?;
    if h.state().state.selected_instance.is_none() {
        return Err(format!("Surface picking did not select geometry: {}", h.state().status));
    }
    h.render()?.save(output.join("scene-selected.png")).map_err(|e| e.to_string())?;
    h.get_by_label("Hide").click();
    h.step();
    settle(&mut h)?;
    if h.state().state.visibility.hidden_instances.len() != 1 {
        return Err("Hide action did not affect the selected instance".into());
    }
    h.get_by_label("Show all").click();
    h.step();
    settle(&mut h)?;
    if !h.state().state.visibility.hidden_instances.is_empty() {
        return Err("Show all did not restore visibility".into());
    }
    h.get_by_label("Isolate selection").click();
    h.step();
    settle(&mut h)?;
    h.get_by_label("Fit selection").click();
    h.step();
    settle(&mut h)?;
    h.render()?.save(output.join("scene-isolated.png")).map_err(|e| e.to_string())?;
    h.state_mut().state.camera.orbit([70., 20.]);
    h.state_mut().state.camera.pan([20., 5.], 700.);
    h.state_mut().state.camera.zoom(100.);
    h.state_mut().changed();
    settle(&mut h)?;
    h.render()?.save(output.join("scene-orbit.png")).map_err(|e| e.to_string())?;
    h.get_by_label("Show all").click();
    h.step();
    settle(&mut h)?;
    if let Some((id, r)) = scene.resources.iter().enumerate().find(|(id, _)| scene.resource_instance_counts.get(*id).is_some_and(|n| *n > 0)) {
        h.state_mut().state.search = r.name.clone();
        h.step();
        let label = format!("{} ({})", r.name.chars().take(120).collect::<String>(), scene.resource_instance_counts.get(id).copied().unwrap_or(0));
        h.get_by_label(&label).click();
        h.step();
        settle(&mut h)?;
        if h.state().state.selected_resource != Some(id) {
            return Err("Resource tree did not select the resource".into());
        }
        h.get_by_label("Hide").click();
        h.step();
        settle(&mut h)?;
        if !h.state().state.visibility.hidden_resources.contains(&id) {
            return Err("Hide resource did not apply its filter".into());
        }
        h.get_by_label("Show all").click();
        h.step();
        settle(&mut h)?;
        h.get_by_label("Isolate selection").click();
        h.step();
        settle(&mut h)?;
        h.get_by_label("Fit selection").click();
        h.step();
        settle(&mut h)?;
        h.render()?.save(output.join("scene-resource.png")).map_err(|e| e.to_string())?;
    }
    let report = serde_json::json!({"schema_version":1,"scene_id":scene.id,"instances":scene.instances.len(),"definitions":scene.meshes.len(),"vertices":scene.positions.len(),"triangles_in_definitions":scene.triangles,"resources":scene.resources.len(),"selected_instance":h.state().state.selected_instance,"isolate_instances":h.state().state.visibility.isolate_instances,"checks":["real bundle load","GPU render","ray surface pick","Hide UI action","Show all UI action","Isolate UI action","Fit selection UI action","camera orbit pan zoom","Resource tree selection and visibility"],"units_verified":false,"axes_verified":false,"materials_verified":false,"native_visual_parity_verified":false});
    std::fs::write(output.join("verification.json"), serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!("Verified viewport; snapshots saved in {}", output.display());
    Ok(())
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let Some((path, output)) = args.get(1).zip(args.get(2)) else {
        eprintln!("Usage: scene_snapshot <scene.json> <output-directory>");
        std::process::exit(2);
    };
    if let Err(e) = snapshot(path.clone(), Path::new(output)) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
