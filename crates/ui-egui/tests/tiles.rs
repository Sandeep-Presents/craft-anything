//! Choice tiles that sit beside each other, in every UI language: the same size, lined up, and in
//! full rows. A program of its own: the UI language is process-wide.

use craft_anything_engine::Mode;
use craft_anything_ui_egui::{CraftApp, demo, i18n};
use egui::Rect;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn harness(lang: &'static str, width: f32) -> Harness<'static, CraftApp> {
    Harness::builder().with_size(egui::vec2(width, 900.0)).with_max_steps(32).build_eframe(move |cc| {
        CraftApp::setup_context(&cc.egui_ctx);
        let mut app = demo::app(demo::engine(Mode::Beginner, true));
        app.settings.language = lang.into();
        i18n::set_language(lang);
        app
    })
}

fn rects(h: &Harness<'static, CraftApp>, labels: &[&str]) -> Vec<Rect> {
    labels.iter().map(|l| h.get_by_role_and_label(egui::accesskit::Role::RadioButton, &i18n::tr(l)).rect()).collect()
}

fn same_size(lang: &str, what: &str, r: &[Rect]) {
    for x in r {
        assert!((x.size() - r[0].size()).length() < 0.5, "{lang}: {what} tiles differ in size: {r:?}");
    }
}

#[test]
fn sibling_tiles_match_in_every_language() {
    let start = ["A website", "An app", "A game", "Start fresh"];
    for lang in i18n::LANGUAGES.iter().map(|l| l.code) {
        // Wide: the four starts in one row.
        let mut h = harness(lang, 1440.0);
        h.run_steps(3);
        let r = rects(&h, &start);
        same_size(lang, "start", &r);
        assert!(r.iter().all(|x| (x.top() - r[0].top()).abs() < 0.5), "{lang}: the four starts aren't one row at 1440: {r:?}");
        // Clean room and Deep, under a picked website: a pair, side by side.
        h.get_by_label(&i18n::tr("A website")).click();
        h.run_steps(3);
        let pair = rects(&h, &["Clean room", "Deep"]);
        same_size(lang, "Clean room / Deep", &pair);
        assert!((pair[0].top() - pair[1].top()).abs() < 0.5, "{lang}: Clean room and Deep aren't side by side: {pair:?}");
        // Narrow: full rows (one of four, or two of two where the labels need the room), never three and one.
        let mut h = harness(lang, 900.0);
        h.run_steps(3);
        let r = rects(&h, &start);
        same_size(lang, "start", &r);
        let rows = [r[0].top(), r[1].top(), r[2].top(), r[3].top()];
        let one_row = rows.iter().all(|y| *y == rows[0]);
        let two_of_two = rows[0] == rows[1] && rows[2] == rows[3] && rows[2] > rows[1];
        assert!(one_row || two_of_two, "{lang}: the starts at 900 aren't full rows: {r:?}");
    }
}
