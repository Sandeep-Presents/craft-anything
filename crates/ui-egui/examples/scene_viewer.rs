//! Native experimental client: cargo run -p craft-anything-ui-egui --example scene_viewer -- /absolute/bundle/scene.json
#![forbid(unsafe_code)]
use craft_anything_ui_egui::scene_viewer::SceneApp;
fn main() -> eframe::Result {
    let path = std::env::args().nth(1).unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200., 660.]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native("Craft Anything — Scene viewer", options, Box::new(move |cc| Ok(Box::new(SceneApp::new(cc, path)))))
}
