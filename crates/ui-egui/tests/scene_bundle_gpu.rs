//! Requires a graphics adapter; explicitly run this test where native rendering is available.
use craft_anything_ui_egui::scene_viewer::SceneApp;
use egui_kittest::{Harness, kittest::Queryable};
use std::time::{Duration, Instant};
#[test]
#[ignore = "requires a native graphics adapter"]
fn desktop_open_path_embeds_the_same_scene_and_returns_to_workspace() {
    for first_run_done in [true, false] {
        desktop_viewer_case(first_run_done);
    }
}
fn desktop_viewer_case(first_run_done: bool) {
    use craft_anything_engine::{Engine, Session, Settings};
    use craft_anything_ui_egui::{CraftApp, demo};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    for ((name, _), bytes) in craft_anything_formats::scene_bundle::MEMBERS.iter().zip(craft_anything_testkit::scene_bundle::fixture()) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let path = dir.path().join("scene.json");
    let engine = Arc::new(Engine::new(Session::new(Settings { deep_mode: true, first_run_done, ..Default::default() })));
    let mut h: Harness<CraftApp> = Harness::builder().with_size(egui::vec2(1400., 900.)).wgpu().build_eframe(move |cc| {
        CraftApp::setup_context(&cc.egui_ctx);
        let mut app = demo::app(engine);
        app.open_path(path);
        app
    });
    let start = Instant::now();
    let mut quiet = 0;
    loop {
        h.step();
        if let Some(scene) = &h.state().scene_viewer {
            assert!(scene.error().is_none(), "{:?}", scene.error());
            if scene.ready() {
                quiet += 1;
            } else {
                quiet = 0;
            }
        }
        if quiet >= 8 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.state().scene_viewer.as_ref().unwrap().model.as_ref().unwrap().scene.instances.len(), 2);
    h.get_by_label("Back to workspace").click();
    h.step();
    assert!(!h.state().state.scene_viewer_open);
    if first_run_done {
        h.get_by_label("Project viewer").click();
        h.step();
        assert!(h.state().state.scene_viewer_open);
    } else {
        assert!(!h.state().settings.first_run_done);
    }
}
fn settle(h: &mut Harness<SceneApp>) {
    let start = Instant::now();
    let mut quiet = 0;
    loop {
        h.step();
        assert!(h.state().error().is_none(), "{:?}", h.state().error());
        if h.state().ready() {
            quiet += 1;
        } else {
            quiet = 0;
        }
        if quiet >= 4 {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "Viewport did not settle: {}", h.state().status);
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn drag(h: &mut Harness<SceneApp>, button: egui::PointerButton) {
    let start = egui::pos2(850., 450.);
    let end = start + egui::vec2(40., 20.);
    h.event(egui::Event::PointerMoved(start));
    h.step();
    h.event(egui::Event::PointerButton { pos: start, button, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::PointerMoved(end));
    h.step();
    h.event(egui::Event::PointerButton { pos: end, button, pressed: false, modifiers: egui::Modifiers::NONE });
    settle(h);
}
#[test]
#[ignore = "requires a native graphics adapter"]
fn scene_settles_and_selection_visibility_changes_rendered_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let files = craft_anything_testkit::scene_bundle::fixture();
    for ((name, _), bytes) in craft_anything_formats::scene_bundle::MEMBERS.iter().zip(files) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let path = dir.path().join("scene.json").to_string_lossy().into_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1400., 900.)).wgpu().build_eframe(move |cc| SceneApp::new(cc, path));
    settle(&mut h);
    assert_eq!(h.state().model.as_ref().unwrap().scene.instances.len(), 2);
    h.state_mut().state.camera.target = [0., 0., 2.];
    h.state_mut().changed();
    settle(&mut h);
    h.state_mut().pick([0., 0.], 1.5);
    settle(&mut h);
    assert_eq!(h.state().state.selected_instance, Some(1));
    h.get_by_label("Isolate selection").click();
    settle(&mut h);
    h.get_by_label("Fit selection").click();
    settle(&mut h);
    let before = h.render().unwrap();
    h.get_by_label("Hide").click();
    settle(&mut h);
    assert!(h.state().state.visibility.hidden_instances.contains(&1));
    let after = h.render().unwrap();
    let changed = (170..800).flat_map(|y| (400..1300).map(move |x| (x, y))).filter(|(x, y)| before.get_pixel(*x, *y) != after.get_pixel(*x, *y)).count();
    assert!(changed > 100, "Hiding the isolated instance must remove rendered geometry");
    h.get_by_label("Show all").click();
    settle(&mut h);
    assert!(h.state().state.visibility.hidden_instances.is_empty());
    h.get_by_label("Synthetic assembly (2)").click();
    settle(&mut h);
    assert_eq!(h.state().state.selected_resource, Some(0));
    h.get_by_label("Hide").click();
    settle(&mut h);
    assert!(h.state().state.visibility.hidden_resources.contains(&0));
    let yaw = h.state().state.camera.yaw;
    drag(&mut h, egui::PointerButton::Primary);
    assert_ne!(h.state().state.camera.yaw, yaw, "Pointer drag must orbit the camera");
    let target = h.state().state.camera.target;
    drag(&mut h, egui::PointerButton::Secondary);
    assert_ne!(h.state().state.camera.target, target, "Right drag must pan the camera");
    let distance = h.state().state.camera.distance;
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0., 80.),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    settle(&mut h);
    assert_ne!(h.state().state.camera.distance, distance, "Wheel input must zoom the camera");
}

#[test]
#[ignore = "requires a native graphics adapter"]
fn scene_recovers_after_rejected_frame_without_reopening() {
    let dir = tempfile::tempdir().unwrap();
    for ((name, _), bytes) in craft_anything_formats::scene_bundle::MEMBERS.iter().zip(craft_anything_testkit::scene_bundle::fixture()) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let path = dir.path().join("scene.json").to_string_lossy().into_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1400., 900.)).wgpu().build_eframe(move |cc| SceneApp::new(cc, path));
    settle(&mut h);
    let source_id = h.state().model.as_ref().unwrap().scene.id.clone();
    let camera = h.state().state.camera.clone();
    let before = h.render().unwrap();
    // This is a deterministic planning error, before GPU encoding, using public client state.
    h.state_mut().state.camera.distance = 0.;
    h.state_mut().changed();
    let started = Instant::now();
    loop {
        h.step();
        if let Some(error) = h.state().error() {
            assert!(error.contains("Invalid camera"), "Unexpected frame failure: {error}");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(15), "Rejected frame did not report an error");
        std::thread::sleep(Duration::from_millis(10));
    }
    h.state_mut().state.camera = camera;
    // A corrected filter must render through the retained worker-owned GPU without opening again.
    h.state_mut().state.visibility.hidden_instances.insert(1);
    h.state_mut().changed();
    settle(&mut h);
    assert_eq!(h.state().model.as_ref().unwrap().scene.id, source_id);
    let after = h.render().unwrap();
    let changed = (170..800).flat_map(|y| (400..1300).map(move |x| (x, y))).filter(|(x, y)| before.get_pixel(*x, *y) != after.get_pixel(*x, *y)).count();
    assert!(changed > 100, "A valid frame after rejection must update rendered geometry");
    h.state_mut().state.visibility.hidden_instances.clear();
    h.state_mut().changed();
    settle(&mut h);
    assert!(h.state().error().is_none(), "A successful recovered frame must clear the current render error");
}

#[test]
#[ignore = "requires a native graphics adapter"]
fn large_controls_do_not_trigger_an_endless_busy_idle_resize_loop() {
    let dir = tempfile::tempdir().unwrap();
    for ((name, _), bytes) in craft_anything_formats::scene_bundle::MEMBERS.iter().zip(craft_anything_testkit::scene_bundle::fixture()) {
        std::fs::write(dir.path().join(name), bytes).unwrap();
    }
    let path = dir.path().join("scene.json").to_string_lossy().into_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1400., 900.)).wgpu().build_eframe(move |cc| {
        let app = SceneApp::new(cc, path);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.interact_size.y = 48.;
            style.spacing.button_padding.y = 12.;
        });
        app
    });
    settle(&mut h);
    let stats = h.state().draw_stats().unwrap().0.clone();
    for _ in 0..12 {
        h.step();
        assert!(h.state().ready(), "An idle viewport must stay idle without new input");
    }
    assert_eq!(h.state().draw_stats().unwrap().0, &stats);
}
