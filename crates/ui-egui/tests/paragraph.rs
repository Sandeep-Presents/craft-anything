//! Inline glossary hints survive shaping a whole paragraph, including Unicode before the term.
use craft_anything_engine::{Mode, feed::FeedItem};
use craft_anything_ui_egui::{CraftApp, demo};
use egui_kittest::{Harness, kittest::Queryable};

#[test]
fn a_formatted_glossary_word_after_unicode_still_has_its_hover_hint() {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 820.0)).build_eframe(|cc| {
        CraftApp::setup_context(&cc.egui_ctx);
        let mut app = demo::app(demo::engine(Mode::Beginner, true));
        demo::fill_project(&mut app);
        app.state.feed = vec![FeedItem::Agent { text: "ação **IPC** e `source.rs`".into(), streaming: false }];
        cc.egui_ctx.all_styles_mut(|style| style.interaction.tooltip_delay = 0.0);
        app
    });
    h.run_steps(3);
    let text = h
        .output()
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text().starts_with("ação IPC") => Some(text),
            _ => None,
        })
        .expect("the formatted paragraph is painted");
    let start = text.galley.pos_from_cursor(egui::text::CCursor { index: egui::text::CharIndex(5), prefer_next_row: true });
    let end = text.galley.pos_from_cursor(egui::text::CCursor { index: egui::text::CharIndex(8), prefer_next_row: false });
    let pointer = egui::pos2((start.left() + end.left()) / 2.0, start.center().y) + text.pos.to_vec2();
    h.input_mut().events.push(egui::Event::PointerMoved(pointer));
    // Tooltips wait for pointer stillness as well as tooltip_delay in egui 0.37.
    h.run_steps(24);
    h.get_by_label("Inter-process communication: how two parts of an app (or two apps) send messages to each other.");
}
