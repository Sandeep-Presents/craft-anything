//! A build's activity, line by line: the "More detail" box in the feed, and the same lines in a
//! window of their own. Commands and paths run long, so a line either wraps or the box scrolls
//! sideways (the person picks, from the buttons that show on hover); the box keeps the height the
//! person drags it to; and the window keeps up with the build while the feed scrolls on.

use crate::i18n::{tr, trf};
use crate::theme::{self, Tokens};
use crate::{CraftApp, icons, widgets};
use craft_anything_ai::rebuild::RebuildRun;
use craft_anything_ai::rebuild::activity::{ActivityLine, LineKind};
use egui::{Color32, Id, Margin, RichText, Sense, Stroke, Ui, Vec2};
use std::path::PathBuf;

/// The height of one line that doesn't wrap. Rows are exactly this tall, or the scroll area
/// miscounts them.
pub const LINE: f32 = 18.0;
/// How many lines the box shows until the person drags it taller or shorter.
const SHOWN_LINES: f32 = 10.0;
/// The shortest it can be dragged to.
const FEWEST_LINES: f32 = 3.0;
/// How much of the window's height it may take.
const MOST_OF_WINDOW: f32 = 0.8;
/// The hover buttons' size, and the grip's height.
const BUTTON: f32 = 24.0;
const GRIP: f32 = 10.0;

/// How one box looks: the person's choices for it, kept while the app runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub height: f32,
    pub wrap: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look { height: LINE * SHOWN_LINES, wrap: false }
    }
}

impl Look {
    pub fn of(ctx: &egui::Context, id: Id) -> Look {
        ctx.data(|d| d.get_temp(id.with("look"))).unwrap_or_default()
    }

    fn keep(self, ctx: &egui::Context, id: Id) {
        ctx.data_mut(|d| d.insert_temp(id.with("look"), self));
    }
}

/// A window with a build's activity: the project's folder (which run) and name (its title).
#[derive(Clone, Debug, PartialEq)]
pub struct PoppedOut {
    pub root: PathBuf,
    pub name: String,
}

/// The box in the feed, for the build in `id`. True when the person asked for its own window.
pub fn inline(ui: &mut Ui, run: &RebuildRun, id: Id) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut look = Look::of(ui.ctx(), id);
    let most = (ui.ctx().content_rect().height() * MOST_OF_WINDOW).max(LINE * FEWEST_LINES);
    let height = look.height.clamp(LINE * FEWEST_LINES, most);
    let card = widgets::card(&t).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        lines(ui, run, id, look.wrap, height, false)
    });
    let popped = toolbar(ui, card.response.rect, id, &mut look, run, true);
    grip(ui, id, &mut look, card.inner, most);
    look.keep(ui.ctx(), id);
    popped
}

/// The lines in a scroll area `height` tall (or shorter when there are fewer, unless `fill`),
/// kept at the bottom as lines arrive. Returns the height it took.
fn lines(ui: &mut Ui, run: &RebuildRun, id: Id, wrap: bool, height: f32, fill: bool) -> f32 {
    let t = Tokens::get(ui.ctx());
    let count = run.activity_len();
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().interact_size.y = LINE;
    // Its own space: a scroll area takes what's left of the one it's in, and at the bottom of a
    // long feed that's nothing, so it got egui's 64-point floor (about three lines) and the
    // rows under it were drawn over it.
    ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
        let area = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() }
            .id_salt(id.with(if wrap { "wrapped" } else { "lines" }))
            .max_height(height)
            .auto_shrink([false, !fill])
            .stick_to_bottom(true);
        let shown = if wrap {
            // Wrapped lines differ in height, so they're all laid out (the run keeps a thousand).
            area.show(ui, |ui| {
                ui.set_width(ui.available_width());
                for line in run.activity_lines(0..count) {
                    row(ui, &line, &t, true);
                }
            })
            .inner_rect
        } else {
            area.show_rows(ui, LINE, count, |ui, range| {
                for line in run.activity_lines(range) {
                    row(ui, &line, &t, false);
                }
            })
            .inner_rect
        };
        shown.height()
    })
    .inner
}

fn row(ui: &mut Ui, line: &ActivityLine, t: &Tokens, wrap: bool) {
    let (colour, words) = colours(line.kind, t);
    let mono = theme::mono(theme::CAPTION);
    let parts = |ui: &mut Ui| {
        ui.label(RichText::new(crate::usage::duration_words(line.at)).font(mono.clone()).color(t.text_faint));
        ui.label(RichText::new(line.kind.tag()).font(mono.clone()).color(colour));
        let text = egui::Label::new(RichText::new(&line.text).font(mono.clone()).color(words));
        ui.add(if wrap { text.wrap() } else { text.extend() });
    };
    if wrap {
        ui.horizontal_top(parts);
    } else {
        ui.horizontal(|ui| {
            ui.set_height(LINE);
            parts(ui);
        });
    }
}

fn colours(kind: LineKind, t: &Tokens) -> (Color32, Color32) {
    match kind {
        LineKind::Failed | LineKind::Problem => (t.warning, t.text),
        LineKind::Done => (t.success, t.text_dim),
        LineKind::Stage => (t.accent_ink, t.text),
        LineKind::Started | LineKind::Said => (t.text_dim, t.text),
        LineKind::Note | LineKind::Output => (t.text_faint, t.text_dim),
    }
}

/// The lines as plain text, the way the box shows them.
pub fn as_text(lines: &[ActivityLine]) -> String {
    lines.iter().map(|l| format!("{}  {}  {}\n", crate::usage::duration_words(l.at), l.kind.tag(), l.text)).collect()
}

/// Wrap, copy and (in the feed) open in a window: in the top right corner of `rect`. In the feed
/// they show while the pointer is over the box or one of them has keyboard focus, so a quiet box
/// stays quiet; in the window, always. True when "own window" was pressed.
fn toolbar(ui: &mut Ui, rect: egui::Rect, id: Id, look: &mut Look, run: &RebuildRun, pop_out: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let focus_id = id.with("toolbar-focus");
    let had_focus = ui.data(|d| d.get_temp::<bool>(focus_id)).unwrap_or(false);
    let shown = ui.ctx().animate_bool_with_time(id.with("toolbar"), !pop_out || had_focus || ui.rect_contains_pointer(rect), 0.12);
    let n = if pop_out { 3.0 } else { 2.0 };
    let size = Vec2::new(BUTTON * n + 2.0 * (n - 1.0) + 6.0, BUTTON + 6.0);
    // Clear of the scroll bar along the right edge: a press on a button over its track also
    // scrolled the lines to where it was pressed.
    let s = &ui.spacing().scroll;
    let bar_room = s.bar_width + s.bar_inner_margin + s.bar_outer_margin;
    let bar = egui::Rect::from_min_size(egui::pos2(rect.right() - size.x - 10.0 - bar_room, rect.top() + 6.0), size);
    let (mut popped, mut focused) = (false, false);
    // A child that takes no room: drawn over the lines, it mustn't move what comes after the box.
    {
        let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)));
        ui.set_opacity(shown);
        // Over the lines it covers, so the icons read against the card, not the text.
        ui.painter().rect(bar, t.radius_sm, t.raised, Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(3.0);
        if pop_out {
            let r = icons::button(ui, "external-link", BUTTON, false, "Open in its own window");
            popped = r.clicked();
            focused |= r.has_focus();
        }
        let r = icons::button(ui, "copy", BUTTON, false, "Copy these lines");
        if r.clicked() {
            ui.ctx().copy_text(as_text(&run.activity_lines(0..run.activity_len())));
        }
        focused |= r.has_focus();
        let wrap = look.wrap;
        let r = icons::button(ui, "wrap-text", BUTTON, wrap, "Wrap long lines");
        r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, wrap, tr("Wrap long lines")));
        if r.clicked() {
            look.wrap = !look.wrap;
        }
        focused |= r.has_focus();
    }
    ui.data_mut(|d| d.insert_temp(focus_id, focused));
    popped
}

/// The edge under the box: drag it to make the box taller or shorter (arrow keys too, when it
/// has focus), double-click it for the usual height. `shown` is the height the lines took.
fn grip(ui: &mut Ui, id: Id, look: &mut Look, shown: f32, most: f32) {
    let t = Tokens::get(ui.ctx());
    let fewest = LINE * FEWEST_LINES;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), GRIP), Sense::click_and_drag());
    let resp = widgets::named(resp, "Drag to change the height");
    if resp.dragged() {
        // From what shows: a box with fewer lines than its height starts from where its edge is.
        look.height = (shown.min(look.height) + resp.drag_delta().y).clamp(fewest, most);
    }
    if resp.has_focus() {
        // egui moves focus with the arrow keys; up and down stay here while it has focus.
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter { vertical_arrows: true, ..Default::default() }));
        let (down, up) =
            ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)));
        let lines = f32::from(u8::from(down)) - f32::from(u8::from(up));
        look.height = (look.height.clamp(fewest, most) + lines * LINE).clamp(fewest, most);
    }
    if resp.double_clicked() {
        look.height = Look::default().height;
    }
    let lit = ui.ctx().animate_bool_with_time(id.with("grip"), resp.hovered() || resp.dragged() || resp.has_focus(), 0.12);
    let colour = theme::mix(t.text_faint.gamma_multiply(0.5), t.text_dim, lit);
    for dy in [-1.5, 1.5] {
        let y = rect.center().y + dy;
        ui.painter().hline(rect.center().x - 12.0..=rect.center().x + 12.0, y, Stroke::new(1.0, colour));
    }
    widgets::focus_ring(ui, &resp, rect);
    resp.on_hover_cursor(egui::CursorIcon::ResizeVertical);
}

/// Each build's activity window that's open. A window goes when it's closed, or when its project
/// has no run any more.
pub fn windows(app: &mut CraftApp, ctx: &egui::Context) {
    let mut closed = Vec::new();
    let mut open_log = None;
    for (i, w) in app.activity_windows.iter().enumerate() {
        let Some(run) = app.runs.get(&w.root) else {
            closed.push(i);
            continue;
        };
        let running = app.runs.is_busy(&w.root);
        let id = Id::new(("activity-window", &w.root));
        let title = trf("{name}: build activity", &[("name", &w.name)]);
        let builder = egui::ViewportBuilder::default().with_title(&title).with_inner_size([820.0, 560.0]).with_min_inner_size([360.0, 220.0]);
        let (close, log) = ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(id), builder, |ui, class| {
            let t = Tokens::get(ui.ctx());
            let embedded = class == egui::ViewportClass::EmbeddedWindow;
            let mut close = ui.input(|i| i.viewport().close_requested());
            let mut log = false;
            let frame = if embedded { egui::Frame::new() } else { egui::Frame::new().fill(t.canvas).inner_margin(Margin::same(12)) };
            frame.show(ui, |ui| {
                if embedded {
                    ui.set_min_size(Vec2::new(520.0, 360.0));
                }
                let mut look = Look::of(ui.ctx(), id);
                // The footer's height, kept free under the lines.
                let footer = 44.0;
                let height = (ui.available_height() - footer).max(LINE * FEWEST_LINES);
                let card = widgets::card(&t).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    lines(ui, run, id, look.wrap, height, true)
                });
                toolbar(ui, card.response.rect, id, &mut look, run, false);
                look.keep(ui.ctx(), id);
                ui.horizontal(|ui| {
                    log = widgets::open_log_button(ui, &run.activity_log());
                    if embedded {
                        close |= widgets::secondary(ui, None, "Close").clicked();
                    }
                });
            });
            if running {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
            (close, log)
        });
        if close {
            closed.push(i);
        }
        if log {
            open_log = Some(run.activity_log());
        }
    }
    for i in closed.into_iter().rev() {
        app.activity_windows.remove(i);
    }
    if let Some(log) = open_log {
        app.open_log(&log);
    }
}

/// Open the window for the project at `root`, or leave the open one as it is.
pub fn pop_out(app: &mut CraftApp, root: PathBuf, name: String) {
    if !app.activity_windows.iter().any(|w| w.root == root) {
        app.activity_windows.push(PoppedOut { root, name });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn copied_lines_read_as_the_box_shows_them() {
        let line = |kind, text: &str| ActivityLine { at: Duration::from_secs(5), kind, text: text.into() };
        let text = as_text(&[line(LineKind::Started, "cargo test --quiet"), line(LineKind::Failed, "exit 101")]);
        let time = crate::usage::duration_words(Duration::from_secs(5));
        assert_eq!(text, format!("{time}  start  cargo test --quiet\n{time}  FAIL  exit 101\n"));
    }
}
