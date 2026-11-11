//! Requires a graphics adapter; explicitly run this test where native rendering is available.
use craft_anything_ui_egui::scene_viewer::SceneApp;
use egui_kittest::{Harness, kittest::Queryable};
use std::time::{Duration, Instant};
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
