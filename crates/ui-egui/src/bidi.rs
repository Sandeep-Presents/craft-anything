//! Right-to-left text (Arabic, Hebrew, Persian, Urdu…) in labels, truncated lines and the prompt
//! box.
//!
//! egui 0.36 shapes each run of Arabic letters correctly (harfrust joins them and draws the run
//! right to left), but it has no bidirectional reordering yet: every run is placed after the
//! previous one, left to right. Spaces come from a different font than Arabic letters, so each
//! word is its own run, and a sentence came out with its words in reverse order, and mixed
//! Arabic/English lines in a jumble. (egui also gives an Arabic word more glyphs than it has
//! characters, so a galley's glyph counts can't be used to find where its rows break.)
//!
//! The fix here breaks the text into lines itself (measuring words with egui), rewrites each line
//! in visual order with the Unicode Bidirectional Algorithm (UAX #9, from `unicode-bidi`), and
//! lays that out without wrapping, aligned to the paragraph's side. Inside a right-to-left run
//! each word is kept in logical order, because egui already draws a word's letters right to
//! left; only the order of words, spaces and punctuation is reversed, and brackets are mirrored.
//! Text with no right-to-left letters takes the usual path, unchanged.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Align, Color32, FontId, FontSelection, Galley, Layout, Response, Ui, WidgetText};
use unicode_bidi::{BidiInfo, Level};

/// True if `c` is a letter or mark of a right-to-left script that egui shapes as one run with
/// its neighbours (Hebrew, Arabic, Syriac, Thaana, N'Ko, Samaritan, Mandaic and their
/// presentation forms).
fn is_rtl_letter(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF
        | 0xFB1D..=0xFDFF
        | 0xFE70..=0xFEFF
        | 0x10800..=0x10FFF
        | 0x1E800..=0x1EFFF)
}

/// Characters that keep a word together inside a right-to-left run: combining marks and the
/// zero-width (non-)joiners.
fn joins_word(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x200C | 0x200D)
}

/// Explicit marks that make text right to left without a right-to-left letter: the
/// right-to-left mark, embedding, override and isolate.
fn is_rtl_mark(c: char) -> bool {
    matches!(c, '\u{200F}' | '\u{202B}' | '\u{202E}' | '\u{2067}')
}

/// True if `text` has any right-to-left letter or mark (only then is the bidi path needed).
pub fn has_rtl(text: &str) -> bool {
    text.chars().any(|c| is_rtl_letter(c) || is_rtl_mark(c))
}

/// True if the first paragraph of `text` reads right to left: its first strong character is RTL,
/// a right-to-left letter or a mark such as U+200F, as the layout decides it.
pub fn is_rtl_paragraph(text: &str) -> bool {
    BidiInfo::new(text, None).paragraphs.first().is_some_and(|p| p.level.is_rtl())
}

/// The bracket or quote that faces the other way (`(` and `)`, `«` and `»`…), for brackets drawn
/// in a right-to-left run; any other character as it is.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        _ => c,
    }
}

/// One character of a line in drawing order: where it is in the text, whether to draw it
/// mirrored, and whether it sits in a right-to-left run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visual {
    at: usize,
    mirrored: bool,
    rtl: bool,
}

/// The characters of the line `line` of `text` in the order they're drawn, left to right. A
/// right-to-left word stays in logical order (egui draws it right to left itself).
fn visual_order(text: &str, bidi: &BidiInfo<'_>, line: Range<usize>) -> Vec<Visual> {
    let mut out = Vec::new();
    let Some(para) = bidi.paragraphs.iter().find(|p| p.range.start <= line.start && line.end <= p.range.end) else {
        // Not inside one paragraph (shouldn't happen): keep the logical order.
        out.extend(text.get(line.clone()).unwrap_or_default().char_indices().map(|(i, _)| Visual { at: line.start + i, mirrored: false, rtl: false }));
        return out;
    };
    if line.is_empty() {
        return out;
    }
    let (levels, runs) = bidi.visual_runs(para, line);
    for run in runs {
        let Some(run_text) = text.get(run.clone()) else { continue };
        let rtl = levels.get(run.start).is_some_and(|l| l.is_rtl());
        if !rtl {
            out.extend(run_text.char_indices().map(|(i, _)| Visual { at: run.start + i, mirrored: false, rtl: false }));
            continue;
        }
        // Split the run into words (kept whole) and single characters, then reverse them.
        let mut atoms: Vec<Vec<Visual>> = Vec::new();
        let mut in_word = false;
        for (i, c) in run_text.char_indices() {
            let at = run.start + i;
            let letter = is_rtl_letter(c) || (in_word && joins_word(c));
            match atoms.last_mut() {
                Some(word) if letter && in_word => word.push(Visual { at, mirrored: false, rtl: true }),
                _ => atoms.push(vec![Visual { at, mirrored: !letter && mirror(c) != c, rtl: true }]),
            }
            in_word = letter;
        }
        out.extend(atoms.into_iter().rev().flatten());
    }
    out
}

/// The character starting at byte `at` of `text`, if there is one.
fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(at..).and_then(|t| t.chars().next())
}

/// `text` (one line per paragraph) in visual order, as egui should be handed it.
pub fn visual_line(text: &str) -> String {
    if !has_rtl(text) {
        return text.to_owned();
    }
    let bidi = BidiInfo::new(text, None);
    let mut s = String::new();
    for (i, para) in bidi.paragraphs.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        let end = text.get(para.range.clone()).map_or(para.range.end, |p| para.range.start + p.trim_end_matches(['\n', '\r']).len());
        for v in visual_order(text, &bidi, para.range.start..end) {
            if let Some(c) = char_at(text, v.at) {
                s.push(if v.mirrored { mirror(c) } else { c });
            }
        }
    }
    s
}

/// The part of `job` in `range`, as a job of its own (no wrapping).
fn sub_job(job: &LayoutJob, range: Range<usize>) -> LayoutJob {
    let mut out = LayoutJob { text: job.text.get(range.clone()).unwrap_or_default().to_owned(), ..LayoutJob::default() };
    for s in &job.sections {
        let (a, b) = (s.byte_range.start.0.max(range.start), s.byte_range.end.0.min(range.end));
        if a < b {
            out.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(a - range.start)..ByteIndex(b - range.start),
                format: s.format.clone(),
            });
        }
    }
    out.wrap.max_width = f32::INFINITY;
    out
}

/// Breaks `text` into lines no wider than `max_width` (by measuring with egui), returning their
/// byte ranges without trailing spaces or newlines. A word wider than the line stands alone.
fn break_lines(ui: &Ui, job: &LayoutJob, max_width: f32) -> Vec<Range<usize>> {
    let text = job.text.as_str();
    let width = |r: Range<usize>| ui.fonts_mut(|f| f.layout_job(sub_job(job, r))).size().x;
    let mut lines = Vec::new();
    let mut start = 0;
    for para in text.split_inclusive('\n') {
        let end = start + para.len();
        let body = para.trim_end_matches(['\n', '\r']);
        // Words with the spaces after them.
        let mut words: Vec<Range<usize>> = Vec::new();
        let mut word_start = start;
        let mut prev_space = false;
        for (i, c) in body.char_indices() {
            let space = c.is_whitespace();
            if prev_space && !space {
                words.push(word_start..start + i);
                word_start = start + i;
            }
            prev_space = space;
        }
        words.push(word_start..start + body.len());
        let trim = |r: Range<usize>| r.start..r.start + text.get(r.clone()).map_or(0, |s| s.trim_end().len());
        let mut line_start = start;
        let mut line_end = start;
        for w in words {
            let candidate = trim(line_start..w.end);
            if line_end > line_start && max_width.is_finite() && width(candidate) > max_width {
                lines.push(trim(line_start..line_end));
                line_start = w.start;
            }
            line_end = w.end;
        }
        lines.push(trim(line_start..line_end));
        start = end;
    }
    lines
}

/// Appends `c` to `out`, extending the last section when it has the same format.
fn push(out: &mut LayoutJob, c: char, format: &TextFormat) {
    let start = out.text.len();
    out.text.push(c);
    let end = out.text.len();
    match out.sections.last_mut() {
        Some(last) if last.byte_range.end == ByteIndex(start) && last.format == *format => last.byte_range.end = ByteIndex(end),
        _ => out.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(start)..ByteIndex(end), format: format.clone() }),
    }
}

/// A galley laid out in visual order, with what each drawn character is in the original text.
pub struct Laid {
    pub galley: Arc<Galley>,
    /// Whether the first paragraph reads right to left (its rows are aligned right).
    pub rtl: bool,
    /// Per row: each drawn character's byte offset in the original text, and whether it's in a
    /// right-to-left run.
    rows: Vec<Vec<Visual>>,
    /// Per row: where its line starts in the original text (for a caret on an empty row).
    starts: Vec<usize>,
    /// Per row: the characters drawn on it, in drawing order (the galley's row text).
    chars: Vec<Vec<char>>,
    /// Per row: whether its paragraph reads right to left (the row is then aligned right).
    row_rtl: Vec<bool>,
    /// Per row: the space before its text (what aligns a right-to-left row to the right).
    leads: Vec<f32>,
    /// Per row: the width of its text.
    widths: Vec<f32>,
    /// The width rows are aligned in: the wrap width, or the widest row.
    width: f32,
    /// The original text.
    text: String,
}

/// Lays `lines` (byte ranges of `job.text`) out in visual order, one row each.
fn lay_lines(ui: &Ui, job: &LayoutJob, lines: Vec<Range<usize>>, ellipsis: bool) -> Laid {
    lay_lines_in(ui, job, lines, ellipsis, None)
}

/// [`lay_lines`] with the paragraphs' base direction given (`None`: from their first strong
/// letter).
fn lay_lines_in(ui: &Ui, job: &LayoutJob, lines: Vec<Range<usize>>, ellipsis: bool, base: Option<Level>) -> Laid {
    let text = job.text.as_str();
    let bidi = BidiInfo::new(text, base);
    let rtl = bidi.paragraphs.first().is_some_and(|p| p.level.is_rtl());
    let fallback = TextFormat::default();
    // Each byte's section, found once: highlighted code has a section per punctuation mark.
    let mut section_of: Vec<Option<usize>> = vec![None; text.len()];
    for (i, sec) in job.sections.iter().enumerate() {
        for slot in section_of.get_mut(sec.byte_range.start.0..sec.byte_range.end.0.min(text.len())).unwrap_or_default() {
            if slot.is_none() {
                *slot = Some(i);
            }
        }
    }
    let format_at = |at: usize| section_of.get(at).copied().flatten().and_then(|i| job.sections.get(i)).map_or(&fallback, |s| &s.format);

    // Each paragraph is aligned to its own side (an English paragraph left, an Arabic one right):
    // rows are laid out aligned left, and a right-to-left row gets the space before it.
    let mut out = LayoutJob { first_row_min_height: job.first_row_min_height, halign: Align::LEFT, ..LayoutJob::default() };
    out.wrap.max_width = f32::INFINITY;
    let mut rows = Vec::with_capacity(lines.len());
    let mut starts = Vec::with_capacity(lines.len());
    let mut chars = Vec::with_capacity(lines.len());
    let mut row_rtl = Vec::with_capacity(lines.len());
    let mut row_section: Vec<Option<usize>> = Vec::with_capacity(lines.len());
    let count = lines.len();
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            let format = out.sections.last().map_or_else(TextFormat::default, |s| s.format.clone());
            push(&mut out, '\n', &format);
        }
        // Each row's text starts a section of its own, which can carry the space before it.
        let first_section = out.sections.len();
        let first_byte = out.text.len();
        let order = visual_order(text, &bidi, line.clone());
        let mut drawn = Vec::with_capacity(order.len() + 1);
        // A cut line ends with "…" where its text goes on: on the left when it reads right to left.
        let mark = ellipsis && i + 1 == count;
        if mark && rtl {
            push(&mut out, '…', format_at(line.start));
            drawn.push('…');
        }
        for v in &order {
            if let Some(c) = char_at(text, v.at) {
                let c = if v.mirrored { mirror(c) } else { c };
                push(&mut out, c, format_at(v.at));
                drawn.push(c);
            }
        }
        if mark && !rtl {
            push(&mut out, '…', format_at(line.end.saturating_sub(1)));
            drawn.push('…');
        }
        // An empty row (an empty line) takes the first paragraph's side, as the caret does.
        let para_rtl = bidi.paragraphs.iter().find(|p| p.range.contains(&line.start)).is_some_and(|p| p.level.is_rtl());
        row_rtl.push(if drawn.is_empty() { rtl } else { para_rtl });
        row_section.push(split_at(&mut out, first_section, first_byte));
        rows.push(order);
        starts.push(line.start);
        chars.push(drawn);
    }
    if out.text.is_empty() {
        out.sections.clear();
        out.append("", 0.0, job.sections.first().map(|s| s.format.clone()).unwrap_or_default());
    }
    let plain = ui.fonts_mut(|f| f.layout_job(out.clone()));
    let widths: Vec<f32> = (0..rows.len()).map(|r| plain.rows.get(r).map_or(0.0, |row| row.rect().width())).collect();
    let widest = widths.iter().copied().fold(0.0_f32, f32::max);
    let width = if job.wrap.max_width.is_finite() { job.wrap.max_width.max(widest) } else { widest };
    let leads: Vec<f32> = widths.iter().zip(&row_rtl).map(|(&w, &r)| if r { (width - w).max(0.0) } else { 0.0 }).collect();
    let galley = if leads.iter().all(|&l| l <= 0.0) {
        plain
    } else {
        for (section, &lead) in row_section.iter().zip(&leads) {
            if let Some(sec) = section.and_then(|i| out.sections.get_mut(i)) {
                sec.leading_space = lead;
            }
        }
        ui.fonts_mut(|f| f.layout_job(out))
    };
    Laid { galley, rtl, rows, starts, chars, row_rtl, leads, widths, width, text: text.to_owned() }
}

/// Makes the text `out` gained from byte `first_byte` on (a row's text) start a section of its own
/// (the first one from `first_section` on), so the row can carry the space before it. Returns
/// that section, or `None` for a row with no text.
fn split_at(out: &mut LayoutJob, first_section: usize, first_byte: usize) -> Option<usize> {
    if out.text.len() <= first_byte {
        return None;
    }
    // The row's first character may have joined the previous row's last section.
    let joined = first_section.checked_sub(1).filter(|&i| out.sections.get(i).is_some_and(|s| s.byte_range.end.0 > first_byte));
    if let Some(i) = joined {
        let sec = out.sections.get_mut(i)?;
        let tail = LayoutSection { leading_space: 0.0, byte_range: ByteIndex(first_byte)..sec.byte_range.end, format: sec.format.clone() };
        sec.byte_range.end = ByteIndex(first_byte);
        out.sections.insert(i + 1, tail);
        return Some(i + 1);
    }
    Some(first_section)
}

/// Lays `job` out with its lines in visual order, wrapped at `job.wrap.max_width`. Text without
/// right-to-left letters is laid out as it is.
pub fn layout(ui: &Ui, job: LayoutJob) -> (Arc<Galley>, bool) {
    layout_in(ui, job, None)
}

/// [`layout`] with the paragraphs' base direction given (`None`: from their first strong letter).
fn layout_in(ui: &Ui, job: LayoutJob, base: Option<Level>) -> (Arc<Galley>, bool) {
    if !has_rtl(&job.text) {
        return (ui.fonts_mut(|f| f.layout_job(job)), false);
    }
    let lines = break_lines(ui, &job, job.wrap.max_width);
    let laid = lay_lines_in(ui, &job, lines, false, base);
    (laid.galley, laid.rtl)
}

/// Lays `job` out on one line in visual order, cut with "…" (at its logical end) to fit
/// `max_width`.
fn layout_truncated(ui: &Ui, job: &LayoutJob, max_width: f32) -> Laid {
    let text = job.text.as_str();
    let first = text.split('\n').next().unwrap_or_default();
    let all = 0..first.trim_end().len();
    let width = |r: Range<usize>| ui.fonts_mut(|f| f.layout_job(sub_job(job, r))).size().x;
    if !max_width.is_finite() || width(all.clone()) <= max_width && first.len() == text.len() {
        return lay_lines(ui, job, std::iter::once(all).collect(), false);
    }
    // Drop words from the end until what's left, with "…", fits.
    let ends: Vec<usize> = first.char_indices().filter(|&(_, c)| c.is_whitespace()).map(|(i, _)| i).collect();
    let dots = ui
        .fonts_mut(|f| {
            f.layout_job(LayoutJob::simple_singleline(
                "…".into(),
                job.sections.first().map_or_else(FontId::default, |s| s.format.font_id.clone()),
                Color32::WHITE,
            ))
        })
        .size()
        .x;
    let fits = |end: usize| width(0..end) + dots <= max_width;
    let by_word = ends.iter().rev().map(|&e| first.get(..e).map_or(0, |s| s.trim_end().len())).find(|&end| end > 0 && fits(end));
    // No words fit (or there's one long word): cut at a character, keeping at least one.
    let end = by_word.unwrap_or_else(|| {
        let bounds: Vec<usize> = first.char_indices().map(|(i, _)| i).skip(1).filter(|&i| i <= all.end).collect();
        bounds.iter().rev().copied().find(|&b| fits(b)).or_else(|| bounds.first().copied()).unwrap_or(all.end)
    });
    lay_lines(ui, job, std::iter::once(0..end).collect(), true)
}

/// Shows a galley from [`layout`]. In a column it goes on the right if it reads right to left;
/// in a row it takes its place in the row. Selecting text in it would copy the visual order, so
/// it isn't selectable; messages have a copy button. Screen readers get `logical`, the text in
/// its own order (egui would read them the galley's visual order).
pub fn show(ui: &mut Ui, galley: Arc<Galley>, rtl: bool, logical: &str) -> Response {
    let label = egui::Label::new(galley).selectable(false);
    let response = if rtl && ui.layout().is_vertical() { ui.with_layout(Layout::top_down(Align::Max), |ui| ui.add(label)).inner } else { ui.add(label) };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), logical));
    response
}

/// `text` as a layout job in `ui`'s style, as a label would lay it out.
fn job_of(ui: &Ui, text: WidgetText) -> LayoutJob {
    Arc::unwrap_or_clone(text.into_layout_job(ui.style(), FontSelection::Default, ui.text_valign()))
}

/// Shows right-to-left `text` as a label: wrapped to the available width when `wrap`, with the
/// paragraphs' base direction `base` (`None`: from their first strong letter).
fn rtl_label(ui: &mut Ui, text: WidgetText, wrap: bool, base: Option<Level>) -> Response {
    let logical = text.text().to_owned();
    let mut job = job_of(ui, text);
    job.wrap.max_width = if wrap { ui.available_width().max(1.0) } else { f32::INFINITY };
    let (galley, rtl) = layout_in(ui, job, base);
    show(ui, galley, rtl, &logical)
}

/// `ui.label(text)` that reads right to left when its text does. Text without right-to-left
/// letters goes to `ui.label` unchanged; right-to-left text wraps where `ui.label` would.
pub fn text(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let text: WidgetText = text.into();
    if !has_rtl(text.text()) {
        return ui.label(text);
    }
    match ui.wrap_mode() {
        egui::TextWrapMode::Wrap => rtl_label(ui, text, true, None),
        egui::TextWrapMode::Extend => rtl_label(ui, text, false, None),
        egui::TextWrapMode::Truncate => truncated(ui, text),
    }
}

/// A wrapping label (`Label::new(text).wrap()`) that reads right to left when its text does.
pub fn label(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let text: WidgetText = text.into();
    if !has_rtl(text.text()) {
        return ui.add(egui::Label::new(text).wrap());
    }
    rtl_label(ui, text, true, None)
}

/// A label for source code: like [`text`], but every line reads left to right (a line such as
/// `// مرحبا` keeps its comment marker on the left); Arabic inside strings and comments is still
/// put in order.
pub fn code_text(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let text: WidgetText = text.into();
    if !has_rtl(text.text()) {
        return ui.label(text);
    }
    let wrap = ui.wrap_mode() == egui::TextWrapMode::Wrap;
    rtl_label(ui, text, wrap, Some(Level::ltr()))
}

/// A one-line label cut to fit (`Label::new(text).truncate()`) that reads right to left when its
/// text does; the "…" goes where the text goes on. Hover shows the whole text.
pub fn truncated(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    let text: WidgetText = text.into();
    if !has_rtl(text.text()) {
        return ui.add(egui::Label::new(text).truncate());
    }
    let whole = text.text().to_owned();
    let job = job_of(ui, text);
    let laid = layout_truncated(ui, &job, ui.available_width().max(1.0));
    let cut = laid.galley.text().contains('…') && !whole.contains('…');
    let response = show(ui, laid.galley, laid.rtl, &whole);
    if cut { response.on_hover_text(visual_line(&whole)) } else { response }
}

/// A galley of `text` on one line, cut to `max_width`, in visual order: for code that paints
/// text itself. Text without right-to-left letters is laid out as egui would.
pub fn line_galley(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    if !has_rtl(text) {
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
        return ui.fonts_mut(|f| f.layout_job(job));
    }
    layout_truncated(ui, &job, max_width).galley
}

/// A galley of right-to-left `text` in visual order for a label built by hand: wrapped to the
/// available width, or on one line cut to fit when `truncate`.
pub fn galley(ui: &Ui, text: impl Into<WidgetText>, truncate: bool) -> Arc<Galley> {
    let mut job = job_of(ui, text.into());
    let width = ui.available_width().max(1.0);
    if truncate {
        return layout_truncated(ui, &job, width).galley;
    }
    job.wrap.max_width = width;
    layout(ui, job).0
}

/// `ui.painter().layout_no_wrap(text, font, color)` in visual order.
pub fn layout_no_wrap(ui: &Ui, text: String, font: FontId, color: Color32) -> Arc<Galley> {
    if !has_rtl(&text) {
        return ui.painter().layout_no_wrap(text, font, color);
    }
    line_galley(ui, &text, font, color, f32::INFINITY)
}

/// `ui.painter().text(pos, anchor, text, font, color)` in visual order.
pub fn paint_line(ui: &Ui, pos: egui::Pos2, anchor: egui::Align2, text: impl ToString, font: FontId, color: Color32) -> egui::Rect {
    let text = text.to_string();
    if !has_rtl(&text) {
        return ui.painter().text(pos, anchor, text, font, color);
    }
    let g = line_galley(ui, &text, font, color, f32::INFINITY);
    let rect = anchor.anchor_size(pos, g.size());
    ui.painter().galley(rect.min, g, color);
    rect
}

impl Laid {
    /// Where row `r`'s text is, relative to the galley's position (an empty row: a zero-width
    /// rectangle on its paragraph's side).
    fn row_span(&self, r: usize) -> Option<egui::Rect> {
        let placed = self.galley.rows.get(r)?;
        let rect = placed.rect();
        let left = placed.pos.x + self.leads.get(r).copied().unwrap_or(0.0);
        let width = self.widths.get(r).copied().unwrap_or(0.0);
        let left = if width <= 0.0 && self.row_rtl.get(r).copied().unwrap_or(self.rtl) { placed.pos.x + self.width } else { left };
        Some(egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(left + width, rect.bottom())))
    }

    /// Where to draw the caret before the character at byte `at` of the original text, relative
    /// to the galley's position: an x and the row's vertical range.
    fn caret(&self, ui: &Ui, at: usize) -> Option<(f32, egui::Rangef)> {
        // An empty row (an empty line, or the line after a final newline) starting here: its side.
        let empty = self.rows.iter().zip(&self.starts).position(|(row, &start)| row.is_empty() && start == at);
        if let Some(rect) = empty.and_then(|r| self.row_span(r)) {
            return Some((rect.left(), egui::Rangef::new(rect.top(), rect.bottom())));
        }
        // The drawn character the caret is before, else the last one before it (the caret then
        // goes after it: the end of a line or of the text).
        let mut hit = None;
        let mut before: Option<(usize, usize, bool, usize)> = None;
        for (r, row) in self.rows.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                if v.at == at {
                    hit = Some((r, k, v.rtl, false));
                } else if v.at < at && before.is_none_or(|b| v.at > b.3) {
                    before = Some((r, k, v.rtl, v.at));
                }
            }
        }
        let (r, k, rtl, after) = match (hit, before) {
            (Some(h), _) => h,
            (None, Some((r, k, rtl, _))) => (r, k, rtl, true),
            (None, None) => (0, 0, self.rtl, self.rtl),
        };
        self.edge(ui, r, k, rtl, after)
    }

    /// The byte offset of the caret position nearest to `p` (relative to the galley's position):
    /// where a click or drag at `p` puts the caret.
    fn index_at(&self, ui: &Ui, p: egui::Pos2) -> usize {
        let last = self.galley.rows.len().saturating_sub(1);
        let r = self.galley.rows.iter().position(|row| p.y < row.rect().bottom()).unwrap_or(last);
        let Some(row) = self.rows.get(r) else { return 0 };
        let mut best: Option<(f32, usize)> = None;
        for (k, v) in row.iter().enumerate() {
            let next = v.at + char_at(&self.text, v.at).map_or(1, char::len_utf8);
            for (after, at) in [(false, v.at), (true, next)] {
                if let Some((x, _)) = self.edge(ui, r, k, v.rtl, after) {
                    let d = (x - p.x).abs();
                    if best.is_none_or(|b| d < b.0) {
                        best = Some((d, at));
                    }
                }
            }
        }
        // An empty row: where its line starts.
        best.map_or_else(|| self.starts.get(r).copied().unwrap_or(0).min(self.text.len()), |b| b.1)
    }

    /// Highlight rectangles for the text between bytes `from` and `to` (either order), relative
    /// to the galley's position: one per run of selected characters that sit together on a row.
    fn selection(&self, ui: &Ui, from: usize, to: usize) -> Vec<egui::Rect> {
        let (a, b) = (from.min(to), from.max(to));
        let mut out: Vec<egui::Rect> = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            let mut spans: Vec<egui::Rect> = Vec::new();
            for (k, v) in row.iter().enumerate() {
                if v.at < a || v.at >= b {
                    continue;
                }
                let (Some((x1, y)), Some((x2, _))) = (self.edge(ui, r, k, v.rtl, false), self.edge(ui, r, k, v.rtl, true)) else { continue };
                spans.push(egui::Rect::from_x_y_ranges(x1.min(x2)..=x1.max(x2), y));
            }
            spans.sort_by(|p, q| p.left().total_cmp(&q.left()));
            for span in spans {
                match out.last_mut() {
                    Some(last) if last.top() == span.top() && span.left() <= last.right() + 1.0 => *last = last.union(span),
                    _ => out.push(span),
                }
            }
        }
        out
    }

    /// The x of drawn character `k` of row `r`: its edge before it (in reading order) or after it.
    fn edge(&self, ui: &Ui, r: usize, k: usize, rtl: bool, after: bool) -> Option<(f32, egui::Rangef)> {
        let rect = self.row_span(r)?;
        let line: &[char] = self.chars.get(r).map_or(&[], Vec::as_slice);
        let font = self.galley.job.sections.first().map_or_else(FontId::default, |s| s.format.font_id.clone());
        let width = |chars: &[char]| ui.fonts_mut(|f| f.layout_no_wrap(chars.iter().collect(), font.clone(), Color32::WHITE)).size().x;
        let prefix = |n: usize| width(line.get(..n.min(line.len())).unwrap_or_default());
        let y = egui::Rangef::new(rect.top(), rect.bottom());
        if !rtl {
            // Left to right: before a character is its left edge, after it its right edge.
            return Some((rect.left() + prefix(if after { k + 1 } else { k }), y));
        }
        // Right to left: the character's word is drawn right to left (its first letter on the
        // right), so measure within the word from the word's right edge.
        let row = self.rows.get(r)?;
        let letter = |i: usize| row.get(i).is_some_and(|v| v.rtl && line.get(i).is_some_and(|&c| is_rtl_letter(c) || joins_word(c)));
        let (mut a, mut b) = (k, k + 1);
        if letter(k) {
            while a > 0 && letter(a - 1) && row.get(a - 1).zip(row.get(a)).is_some_and(|(p, q)| p.at < q.at) {
                a -= 1;
            }
            while letter(b) && row.get(b - 1).zip(row.get(b)).is_some_and(|(p, q)| p.at < q.at) {
                b += 1;
            }
        }
        let right = rect.left() + prefix(b);
        let upto = if after { k + 1 } else { k };
        let mut part: Vec<char> = line.get(a..upto.min(line.len()).max(a)).unwrap_or_default().to_vec();
        // Measured alone, the part's last letter would take its final shape, not the joined one it
        // has in the word, and the caret could step backwards; a zero-width joiner keeps it joined
        // (its own width, zero or a missing-glyph box, is taken off).
        let joiner = if !part.is_empty() && upto < b {
            part.push('\u{200D}');
            width(&['\u{200D}'])
        } else {
            0.0
        };
        let inside = width(&part) - joiner;
        Some((right - inside, y))
    }
}

/// A text field's lines: [`break_lines`], plus the empty line after a final newline (Shift+Enter),
/// where the caret then sits.
fn field_lines(ui: &Ui, job: &LayoutJob, wrap: f32) -> Vec<Range<usize>> {
    let mut lines = break_lines(ui, job, wrap);
    if job.text.ends_with('\n') {
        lines.push(job.text.len()..job.text.len());
    }
    lines
}

/// The part of a text field its text may use: from where egui starts the text to the field's
/// right edge less the same margin. Not `text_clip_rect`, which egui sizes to its own text (98
/// points of a 616-point box), so right-to-left text anchored there sat on the left; and not
/// egui's wrap width, which can reach past the box (text then appeared only after ~40 letters).
fn text_area(output: &egui::text_edit::TextEditOutput) -> egui::Rect {
    let field = output.response.response.rect;
    let clip = output.text_clip_rect;
    let margin = (clip.left() - field.left()).max(0.0);
    let right = (field.right() - margin).max(clip.right());
    egui::Rect::from_min_max(clip.min, egui::pos2(right, clip.max.y))
}

/// A text field's text laid out in visual order, and where to paint it.
fn text_edit_layout(ui: &Ui, output: &egui::text_edit::TextEditOutput, text: &str, font: FontId, color: Color32) -> (Laid, egui::Pos2) {
    let area = text_area(output);
    let wrap = area.width().max(1.0);
    let job = LayoutJob::simple(text.to_owned(), font, color, wrap);
    let laid = lay_lines(ui, &job, field_lines(ui, &job, wrap), false);
    // Rows fill the area (each aligned to its paragraph's side). A word wider than the area makes
    // them wider: right-to-left text then stays on the area's right edge and spills to the left.
    let x = if laid.rtl { area.right() - laid.width } else { area.left() };
    (laid, egui::pos2(x, output.galley_pos.y))
}

/// Draws a text field's right-to-left text in visual order over the field, with its caret, when
/// the field was shown with an invisible text colour and caret (see [`text_edit_style`]).
pub fn paint_text_edit(ui: &Ui, output: &egui::text_edit::TextEditOutput, text: &str, font: FontId, color: Color32) {
    if !has_rtl(text) {
        return;
    }
    let area = text_area(output);
    let (laid, pos) = text_edit_layout(ui, output, text, font, color);
    let painter = ui.painter_at(area.expand(2.0));
    let byte = |ci: usize| text.char_indices().nth(ci).map_or(text.len(), |(i, _)| i);
    let range = mouse_selection(ui, output, &laid, pos, text).or(output.cursor_range);
    // The selection, behind the text, in the colour egui would have used (the field's own is hidden).
    if let Some(range) = range
        && range.primary.index != range.secondary.index
    {
        let fill = ui.ctx().global_style().visuals.selection.bg_fill;
        for rect in laid.selection(ui, byte(range.primary.index.0), byte(range.secondary.index.0)) {
            painter.rect_filled(rect.translate(pos.to_vec2()), 0.0, fill);
        }
    }
    painter.galley(pos, laid.galley.clone(), color);
    if output.response.has_focus()
        && let Some(range) = range
    {
        let at = byte(range.primary.index.0);
        if let Some((x, y)) = laid.caret(ui, at) {
            let stroke = ui.visuals().text_cursor.stroke;
            let stroke = egui::Stroke::new(stroke.width.max(1.5), ui.visuals().strong_text_color());
            painter.vline(pos.x + x, (pos.y + y.min)..=(pos.y + y.max), stroke);
        }
    }
}

/// egui places the caret for a click or drag from its own left-to-right layout, so in
/// right-to-left text dragging one way selected letters on the other side. While the mouse button
/// is down on the field, put the caret (and the selection) where the pointer is in the drawn
/// text instead, and store it as the field's cursor. A double click selects the word.
fn mouse_selection(ui: &Ui, output: &egui::text_edit::TextEditOutput, laid: &Laid, pos: egui::Pos2, text: &str) -> Option<egui::text_selection::CCursorRange> {
    use egui::text::CCursor;
    let response = &output.response.response;
    let (pressed, double, shift) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.button_double_clicked(egui::PointerButton::Primary), i.modifiers.shift));
    // egui reports a double click as the button goes up, when it's no longer down on the field.
    let double = double && (response.double_clicked() || response.is_pointer_button_down_on());
    if !response.is_pointer_button_down_on() && !double {
        return None;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos())?;
    let hit_byte = laid.index_at(ui, pointer - pos.to_vec2());
    let chars = |b: usize| text.get(..b).map_or(0, |s| s.chars().count());
    let hit = chars(hit_byte);
    let key = response.id.with("bidi-anchor");
    let range = if double {
        // The word around the click, in the text's own order.
        let start = text.get(..hit_byte).and_then(|s| s.rfind(char::is_whitespace).map(|i| i + 1)).unwrap_or(0);
        let end = text.get(hit_byte..).and_then(|s| s.find(char::is_whitespace)).map_or(text.len(), |i| hit_byte + i);
        ui.data_mut(|d| d.insert_temp(key, chars(start)));
        egui::text_selection::CCursorRange { primary: CCursor::new(chars(end)), secondary: CCursor::new(chars(start)), h_pos: None }
    } else {
        let anchor =
            if pressed && !shift { hit } else { ui.data(|d| d.get_temp::<usize>(key)).or(output.cursor_range.map(|r| r.secondary.index.0)).unwrap_or(hit) };
        if pressed {
            ui.data_mut(|d| d.insert_temp(key, anchor));
        }
        egui::text_selection::CCursorRange { primary: CCursor::new(hit), secondary: CCursor::new(anchor), h_pos: None }
    };
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
        state.cursor.set_char_range(Some(range));
        egui::TextEdit::store_state(ui.ctx(), response.id, state);
    }
    Some(range)
}

/// Swaps the Left and Right arrow keys in `events` (Shift, Ctrl and other modifiers kept).
fn swap_arrow_keys(events: &mut [egui::Event]) {
    let swap = |k: &mut egui::Key| {
        *k = match *k {
            egui::Key::ArrowLeft => egui::Key::ArrowRight,
            egui::Key::ArrowRight => egui::Key::ArrowLeft,
            other => other,
        };
    };
    for event in events {
        if let egui::Event::Key { key, physical_key, .. } = event {
            swap(key);
            if let Some(p) = physical_key {
                swap(p);
            }
        }
    }
}

/// Whether the paragraph holding the caret (character index `caret`) reads right to left: from
/// its own text when it has any (a line of numbers reads left to right, as it's drawn); an empty
/// paragraph takes the text's first paragraph's direction, which its row is aligned to.
fn caret_paragraph_is_rtl(text: &str, caret: usize) -> bool {
    let at = text.char_indices().nth(caret).map_or(text.len(), |(i, _)| i);
    let start = text.get(..at).and_then(|s| s.rfind('\n')).map_or(0, |i| i + 1);
    let end = text.get(at..).and_then(|s| s.find('\n')).map_or(text.len(), |i| at + i);
    let para = text.get(start..end).unwrap_or_default();
    if para.trim().is_empty() { is_rtl_paragraph(text) } else { is_rtl_paragraph(para) }
}

/// Before showing the focused text field `id` holding `text`: when the paragraph holding the
/// caret reads right to left, Left moves the caret forward through the text and Right back (egui
/// moves by the text's order, so Left went right on screen). Shift still selects. Paragraphs that
/// read left to right are left as they are.
pub fn arrow_keys_follow_direction(ui: &Ui, id: egui::Id, text: &str) {
    if !has_rtl(text) || !ui.memory(|m| m.has_focus(id)) {
        return;
    }
    let caret = egui::TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range()).map_or(0, |r| r.primary.index.0);
    if caret_paragraph_is_rtl(text, caret) {
        ui.input_mut(|i| swap_arrow_keys(&mut i.events));
    }
}

/// Before showing a text field holding `text`: when it's right to left, hide egui's own drawing
/// of it (text, caret and selection highlight), which [`paint_text_edit`] draws instead.
pub fn text_edit_style(ui: &mut Ui, text: &str) -> bool {
    if !has_rtl(text) {
        return false;
    }
    let v = ui.visuals_mut();
    v.text_cursor.stroke.color = Color32::TRANSPARENT;
    v.selection.bg_fill = Color32::TRANSPARENT;
    v.selection.stroke.color = Color32::TRANSPARENT;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_words_are_drawn_from_the_right() {
        // egui draws each word right to left; the words themselves must come in reverse order.
        assert_eq!(visual_line("مرحبا بالعالم"), "بالعالم مرحبا");
        assert_eq!(visual_line("هذا تطبيق جميل"), "جميل تطبيق هذا");
        assert_eq!(visual_line("لدي فكرة برنامج"), "برنامج فكرة لدي");
    }

    #[test]
    fn english_and_numbers_inside_arabic_keep_their_order() {
        assert_eq!(visual_line("أنا أحب Rust كثيرا"), "كثيرا Rust أحب أنا");
        assert_eq!(visual_line("أنا أحب Rust and egui كثيرا"), "كثيرا Rust and egui أحب أنا");
        assert_eq!(visual_line("انتظر 72 ساعة"), "ساعة 72 انتظر");
    }

    #[test]
    fn arabic_inside_english_is_reversed_in_place() {
        assert_eq!(visual_line("I said مرحبا بالعالم today"), "I said بالعالم مرحبا today");
    }

    #[test]
    fn brackets_are_mirrored_and_punctuation_follows_the_sentence() {
        assert_eq!(visual_line("(مرحبا)"), "(مرحبا)");
        assert_eq!(visual_line("مرحبا بالعالم."), ".بالعالم مرحبا");
    }

    #[test]
    fn marks_and_joiners_stay_in_their_word() {
        // Shadda + fatha on the first word, a ZWNJ inside the Persian word.
        assert_eq!(visual_line("مُّحمد می\u{200C}خواهم"), "می\u{200C}خواهم مُّحمد");
    }

    #[test]
    fn plain_text_is_left_alone() {
        assert!(!has_rtl("Hello (world) 123"));
        assert_eq!(visual_line("Hello (world)"), "Hello (world)");
        assert!(is_rtl_paragraph("مرحبا Rust"));
        assert!(!is_rtl_paragraph("Rust مرحبا"));
    }

    /// A 400×300 test window with the app's fonts that runs `f` each frame.
    fn harness(f: impl FnMut(&mut Ui) + 'static) -> egui_kittest::Harness<'static> {
        let mut f = f;
        egui_kittest::Harness::builder().with_size(egui::vec2(400.0, 300.0)).build_ui(move |ui| {
            crate::theme::install_fonts(ui.ctx());
            f(ui);
        })
    }

    /// The wrapped layout keeps every character, keeps every row inside the width (egui gives
    /// Arabic words extra glyphs, so rows can't be found from glyph counts) and puts each row in
    /// visual order, aligned right.
    #[test]
    fn wrapped_arabic_keeps_every_character_and_fits() {
        let text = "فهمت أن لديك عدة أسئلة. يسعدني أن أجيب عنها. لم أبدأ بناء أي برنامج بعد، لأن رسالتك لا تحدد بعد ما الذي تريد صنعه (72 ساعة) مع Rust.";
        let mut h = harness(move |ui| {
            let mut job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 180.0);
            job.wrap.max_width = 180.0;
            let (galley, rtl) = layout(ui, job);
            assert!(rtl);
            assert!(galley.rows.len() > 2, "{} rows", galley.rows.len());
            for row in &galley.rows {
                assert!((row.rect().right() - 180.0).abs() < 1.0, "row {:?} isn't aligned right: {:?}", row.text(), row.rect());
            }
            let mut shown: Vec<char> = galley.text().chars().filter(|c| !c.is_whitespace()).collect();
            let mut given: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
            shown.sort_unstable();
            given.sort_unstable();
            assert_eq!(shown, given);
            for row in &galley.rows {
                assert!(row.rect().width() <= 181.0, "row {:?} is {} wide", row.text(), row.rect().width());
            }
            let rows: Vec<&str> = galley.text().lines().collect();
            assert!(rows.first().is_some_and(|r| r.ends_with("فهمت")), "{rows:?}");
            assert!(rows.last().is_some_and(|r| r.starts_with(".Rust")), "{rows:?}");
            assert!(rows.iter().all(|r| r.trim() != "."), "a full stop on a row of its own: {rows:?}");
        });
        h.step();
    }

    /// A cut right-to-left line keeps its first words (on the right) and puts "…" on the left.
    #[test]
    fn a_cut_arabic_line_keeps_its_start() {
        let mut h = harness(|ui| {
            let g = line_galley(ui, "تطبيق سطح مكتب اصلي جديد للتجربة", FontId::proportional(14.0), Color32::WHITE, 90.0);
            let shown = g.text();
            assert!(shown.starts_with('…'), "{shown:?}");
            assert!(shown.ends_with("تطبيق"), "{shown:?}");
            assert!(g.size().x <= 91.0, "{} wide", g.size().x);
            let plain = line_galley(ui, "a website for my bakery", FontId::proportional(14.0), Color32::WHITE, 400.0);
            assert_eq!(plain.text(), "a website for my bakery");
        });
        h.step();
    }

    /// Typing Arabic in the prompt box: the caret at the end sits on the left of the words, and
    /// at the start on their right.
    #[test]
    fn the_caret_follows_right_to_left_typing() {
        let mut h = harness(|ui| {
            let text = "لدي فكرة برنامج";
            let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
            let laid = lay_lines(ui, &job, break_lines(ui, &job, 300.0), false);
            let row = laid.row_span(0).unwrap_or(egui::Rect::NOTHING);
            let end = laid.caret(ui, text.len()).map(|c| c.0).unwrap_or(f32::NAN);
            let start = laid.caret(ui, 0).map(|c| c.0).unwrap_or(f32::NAN);
            assert!((end - row.left()).abs() < 2.0, "end caret {end}, row {row:?}");
            assert!((start - row.right()).abs() < 2.0, "start caret {start}, row {row:?}");
        });
        h.step();
    }

    /// The prompt box's Arabic text is drawn inside the box from the first letter (it was once
    /// anchored past the box's right edge, and showed only after about 40 letters).
    #[test]
    fn prompt_box_text_is_drawn_inside_the_box() {
        let mut h = harness(|ui| {
            for text in ["س", "لدي فكرة برنامج"] {
                let mut s = text.to_owned();
                let output = egui::TextEdit::multiline(&mut s).desired_width(f32::INFINITY).show(ui);
                let (laid, pos) = text_edit_layout(ui, &output, text, FontId::proportional(14.0), Color32::WHITE);
                let drawn = laid.galley.rect.translate(pos.to_vec2());
                let field = output.response.response.rect;
                let area = text_area(&output);
                // egui's first frame can size the box narrower than one letter; a text wider than
                // its box spills to the left, and only a box it fits in must contain it.
                if laid.width <= area.width() + 0.5 {
                    assert!(area.expand(1.0).contains_rect(drawn), "{text:?} drawn at {drawn:?}, box text area {area:?}");
                }
                // On the right of the box itself (less its margin), not of egui's text-sized area.
                assert!((drawn.right() - area.right()).abs() < 1.0, "{text:?} not on the right: {drawn:?} in {area:?}");
                assert!(field.right() - area.right() < 10.0, "{text:?}: text area {area:?} stops short of the box {field:?}");
            }
        });
        h.step();
    }

    /// Selecting Arabic in the prompt box highlights the selected words where they're drawn.
    #[test]
    fn selected_arabic_is_highlighted_where_it_is_drawn() {
        let mut h = harness(|ui| {
            let text = "لدي فكرة برنامج";
            let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
            let laid = lay_lines(ui, &job, break_lines(ui, &job, 300.0), false);
            let row = laid.row_span(0).unwrap_or(egui::Rect::NOTHING);
            // Everything: one rectangle over the whole row.
            let all = laid.selection(ui, 0, text.len());
            assert_eq!(all.len(), 1, "{all:?}");
            assert!(all.iter().all(|r| (r.left() - row.left()).abs() < 2.0 && (r.right() - row.right()).abs() < 2.0), "{all:?} vs {row:?}");
            // The first word ("لدي") is drawn on the right: its highlight is on the right.
            let first = laid.selection(ui, 0, "لدي".len());
            assert_eq!(first.len(), 1, "{first:?}");
            assert!(first.iter().all(|r| (r.right() - row.right()).abs() < 2.0 && r.width() < row.width() / 2.0), "{first:?} vs {row:?}");
            assert!(laid.selection(ui, 3, 3).is_empty());
        });
        h.step();
    }

    /// A click on the left of a right-to-left line puts the caret at the text's end, and on the
    /// right at its start: the reverse of what egui's own layout would say.
    #[test]
    fn a_click_finds_the_letter_where_it_is_drawn() {
        let mut h = harness(|ui| {
            let text = "لدي فكرة برنامج";
            let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
            let laid = lay_lines(ui, &job, break_lines(ui, &job, 300.0), false);
            let row = laid.row_span(0).unwrap_or(egui::Rect::NOTHING);
            assert_eq!(laid.index_at(ui, egui::pos2(row.left() - 5.0, row.center().y)), text.len());
            assert_eq!(laid.index_at(ui, egui::pos2(row.right() + 5.0, row.center().y)), 0);
            // Between the first word (on the right) and the second.
            let first = laid.selection(ui, 0, "لدي".len());
            let left_of_first = first.first().map_or(0.0, |r| r.left());
            let at = laid.index_at(ui, egui::pos2(left_of_first, row.center().y));
            assert!(at == "لدي".len() || at == "لدي ".len(), "{at}");
        });
        h.step();
    }

    /// Every spot on screen where the caret can stand for byte position `at`: before the
    /// character that starts there, or after the one that ends there.
    fn spots(laid: &Laid, ui: &Ui, text: &str, at: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for (r, row) in laid.rows.iter().enumerate() {
            for (k, v) in row.iter().enumerate() {
                let next = v.at + char_at(text, v.at).map_or(1, char::len_utf8);
                for (after, pos) in [(false, v.at), (true, next)] {
                    if pos == at
                        && let Some((x, _)) = laid.edge(ui, r, k, v.rtl, after)
                    {
                        out.push(x);
                    }
                }
            }
        }
        out
    }

    /// Mixed languages: Arabic with English, numbers, brackets, Hebrew and Persian, and English
    /// with Arabic inside. At every position in the text, a click on the drawn caret puts the
    /// caret back at that position, and every caret stays inside its row.
    #[test]
    fn caret_and_clicks_agree_in_mixed_text() {
        let mut h = harness(|ui| {
            let texts = [
                "أنا أحب Rust كثيرا",
                "انتظر 72 ساعة (تقريبا) ثم جرّب egui 0.36 مرة أخرى",
                "I love العربية and עברית a lot",
                "من می\u{200C}خواهم Craft Anything را امتحان کنم",
                "Run cargo build ثم افتح app/ في المتصفح",
            ];
            for text in texts {
                let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 1000.0);
                let laid = lay_lines(ui, &job, break_lines(ui, &job, 1000.0), false);
                let row = laid.row_span(0).unwrap_or(egui::Rect::NOTHING);
                let mut wrong = Vec::new();
                for (at, _) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
                    let Some((x, _)) = laid.caret(ui, at) else {
                        wrong.push(format!("{at}: no caret"));
                        continue;
                    };
                    if x < row.left() - 1.0 || x > row.right() + 1.0 {
                        wrong.push(format!("{at}: caret {x} outside {row:?}"));
                    }
                    let back = laid.index_at(ui, egui::pos2(x, row.center().y));
                    // Where English and Arabic meet, one spot belongs to two positions (here
                    // the text's end and the space after "app/"); either is a right answer.
                    let same_spot = spots(&laid, ui, text, back).iter().any(|bx| (bx - x).abs() < 0.5);
                    if back != at && !same_spot {
                        wrong.push(format!("{at} -> x {x} -> {back}"));
                    }
                }
                assert!(wrong.is_empty(), "{text:?}: {wrong:?}");
            }
            // A paragraph that starts in English reads left to right and stays on the left.
            let mut job = LayoutJob::simple("I love العربية a lot".to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
            job.wrap.max_width = 300.0;
            let (g, rtl) = layout(ui, job);
            assert!(!rtl);
            assert_eq!(g.job.halign, Align::LEFT);
            assert_eq!(g.text(), "I love العربية a lot");
        });
        h.step();
    }

    /// A long name with no spaces is cut at a letter to fit, not shown whole past its space.
    #[test]
    fn a_long_single_word_is_cut_to_fit() {
        let mut h = harness(|ui| {
            for name in ["مشروعطويلجدابدونمسافاتعلىالإطلاق", "اسم مشروعطويلجدابدونمسافاتعلىالإطلاق"]
            {
                let g = line_galley(ui, name, FontId::proportional(14.0), Color32::WHITE, 80.0);
                assert!(g.size().x <= 81.0, "{name:?} is {} wide", g.size().x);
                assert!(g.text().contains('…'), "{:?}", g.text());
            }
        });
        h.step();
    }

    /// Screen readers get a right-to-left label's text in its own order, not the drawn order.
    #[test]
    fn screen_readers_get_the_logical_text() {
        use egui_kittest::kittest::Queryable as _;
        let mut h = harness(|ui| {
            text(ui, "لدي فكرة برنامج");
        });
        h.run();
        let _ = h.get_by_label("لدي فكرة برنامج");
    }

    /// The caret on an empty line in the middle, and on the new line after a final newline
    /// (Shift+Enter in the prompt box), stands on that line, not at the end of the one before.
    #[test]
    fn the_caret_stands_on_empty_lines() {
        let mut h = harness(|ui| {
            for text in ["لدي فكرة\n\nبرنامج", "لدي فكرة\n"] {
                let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
                let laid = lay_lines(ui, &job, field_lines(ui, &job, 300.0), false);
                let at = "لدي فكرة\n".len();
                let first = laid.row_span(0).unwrap_or(egui::Rect::NOTHING);
                let second = laid.row_span(1).unwrap_or(egui::Rect::NOTHING);
                let (x, y) = laid.caret(ui, at).unwrap_or((f32::NAN, egui::Rangef::NOTHING));
                assert!(y.min >= first.bottom() - 0.5 && (y.min - second.top()).abs() < 0.5, "{text:?}: caret rows {y:?}, line 2 {second:?}");
                assert!((x - second.right()).abs() < 0.5, "{text:?}: caret x {x}, line 2 {second:?}");
                assert_eq!(laid.index_at(ui, egui::pos2(x, second.center().y)), at, "{text:?}");
            }
        });
        h.step();
    }

    /// Code keeps reading left to right: a comment marker stays on the left of its Arabic.
    #[test]
    fn code_reads_left_to_right() {
        let mut h = harness(|ui| {
            let job = LayoutJob::simple("// مرحبا بالعالم".to_owned(), FontId::monospace(12.0), Color32::WHITE, f32::INFINITY);
            let (g, rtl) = layout_in(ui, job.clone(), Some(Level::ltr()));
            assert!(!rtl);
            assert_eq!(g.text(), "// بالعالم مرحبا");
            let (auto, auto_rtl) = layout(ui, job);
            assert!(auto_rtl, "without the base direction the line reads right to left: {:?}", auto.text());
        });
        h.step();
    }

    /// In right-to-left text Left and Right swap (so Left moves the caret left on screen), with
    /// Shift kept; in left-to-right text they stay.
    #[test]
    fn arrow_keys_follow_the_drawn_direction() {
        let key = |key, shift| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers { shift, ..egui::Modifiers::NONE },
        };
        let mut events = vec![key(egui::Key::ArrowLeft, false), key(egui::Key::ArrowRight, true), key(egui::Key::A, false)];
        swap_arrow_keys(&mut events);
        assert_eq!(events, vec![key(egui::Key::ArrowRight, false), key(egui::Key::ArrowLeft, true), key(egui::Key::A, false)]);

        // In a real text field: from the text's start (on the right), Left moves one letter on.
        let text = "لدي فكرة";
        let id = egui::Id::new("rtl-field");
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(400.0, 100.0)).build_ui(move |ui| {
            crate::theme::install_fonts(ui.ctx());
            let mut s = text.to_owned();
            ui.memory_mut(|m| m.request_focus(id));
            arrow_keys_follow_direction(ui, id, &s);
            let out = egui::TextEdit::singleline(&mut s).id(id).show(ui);
            let at = out.cursor_range.map(|r| (r.primary.index.0, r.secondary.index.0));
            ui.data_mut(|d| d.insert_temp(id.with("test-cursor"), at));
        });
        h.run();
        let mut state = egui::TextEdit::load_state(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(0))));
        egui::TextEdit::store_state(&h.ctx, id, state);
        h.key_press(egui::Key::ArrowLeft);
        h.run();
        let at = h.ctx.data(|d| d.get_temp::<Option<(usize, usize)>>(id.with("test-cursor"))).flatten();
        assert_eq!(at, Some((1, 1)), "Left from the start should move one letter into the text");
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowLeft);
        h.run();
        let at = h.ctx.data(|d| d.get_temp::<Option<(usize, usize)>>(id.with("test-cursor"))).flatten();
        assert_eq!(at, Some((2, 1)), "Shift+Left should select the next letter");
    }

    /// The arrows follow the paragraph the caret is in: an English line before an Arabic one
    /// keeps its arrows, the Arabic line swaps them; an empty line follows the first paragraph.
    #[test]
    fn arrow_direction_is_the_caret_paragraphs() {
        let text = "Hello world\nلدي فكرة\n\nok";
        assert!(!caret_paragraph_is_rtl(text, 3), "in the English line");
        assert!(caret_paragraph_is_rtl(text, 14), "in the Arabic line");
        assert!(!caret_paragraph_is_rtl(text, 21), "on the empty line: the first paragraph's direction");
        assert!(!caret_paragraph_is_rtl(text, 23), "in the last English line");
        assert!(caret_paragraph_is_rtl("لدي فكرة\n\nok", 9), "an empty line after Arabic first");
        assert!(!caret_paragraph_is_rtl("مرحبا\n123", 8), "a line of numbers reads left to right, as it's drawn");
        assert!(!caret_paragraph_is_rtl("مرحبا\n(1) - 2", 8), "numbers and punctuation only: left to right");
        assert!(caret_paragraph_is_rtl("مرحبا\n123", 2), "the Arabic line above it still swaps");
        assert!(caret_paragraph_is_rtl("مرحبا\n\u{200F}😀😃", 8), "a right-to-left mark makes its line right to left");
        assert!(!caret_paragraph_is_rtl("مرحبا\n\u{200E}😀😃", 8), "a left-to-right mark makes its line left to right");
    }

    /// Pressing Left through a wrapped Arabic paragraph in a real text field moves the painted
    /// caret left along each row and then onto the next row's right end, never right; Down moves
    /// it to a lower row. Checked against the layout the field paints.
    #[test]
    fn left_walks_wrapped_arabic_as_painted() {
        type Seen = Option<(usize, f32, f32)>;
        let text = "لدي فكرة برنامج يقسم يومي إلى نوم ودراسة وبرمجة وترفيه ويغلق المتصفحات";
        let id = egui::Id::new("wrapped-rtl-field");
        let key = id.with("seen");
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(200.0, 200.0)).build_ui(move |ui| {
            crate::theme::install_fonts(ui.ctx());
            let mut s = text.to_owned();
            ui.memory_mut(|m| m.request_focus(id));
            arrow_keys_follow_direction(ui, id, &s);
            let out = egui::TextEdit::multiline(&mut s).id(id).desired_width(150.0).show(ui);
            let font = FontId::proportional(14.0);
            let (laid, _) = text_edit_layout(ui, &out, text, font, Color32::WHITE);
            let seen: Seen = out.cursor_range.and_then(|r| {
                let ci = r.primary.index.0;
                let at = text.char_indices().nth(ci).map_or(text.len(), |(i, _)| i);
                laid.caret(ui, at).map(|(x, y)| (ci, x, y.min))
            });
            ui.data_mut(|d| d.insert_temp(key, (seen, laid.galley.rows.len())));
        });
        h.run();
        let mut state = egui::TextEdit::load_state(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(0))));
        egui::TextEdit::store_state(&h.ctx, id, state);
        h.run();
        let read = |h: &egui_kittest::Harness<'_>| h.ctx.data(|d| d.get_temp::<(Seen, usize)>(key)).unwrap_or((None, 0));
        let (start, rows) = read(&h);
        assert!(rows >= 3, "the text should wrap onto several rows, got {rows}");
        let mut last = start.unwrap_or((0, f32::NAN, f32::NAN));
        let mut row_changes = 0;
        for _ in 0..text.chars().count() {
            h.key_press(egui::Key::ArrowLeft);
            h.run();
            let Some(now) = read(&h).0 else { panic!("no caret after Left") };
            assert_eq!(now.0, last.0 + 1, "Left should move one letter on in the text");
            if (now.2 - last.2).abs() < 0.5 {
                assert!(now.1 <= last.1 + 0.5, "on one row the caret moved right: {last:?} -> {now:?}");
            } else {
                assert!(now.2 > last.2, "the caret went up a row: {last:?} -> {now:?}");
                row_changes += 1;
            }
            last = now;
        }
        assert!(row_changes >= 2, "the walk should cross row boundaries, crossed {row_changes}");

        // Down from the start goes to a lower row.
        let mut state = egui::TextEdit::load_state(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(0))));
        egui::TextEdit::store_state(&h.ctx, id, state);
        h.run();
        let top = read(&h).0.map_or(f32::NAN, |s| s.2);
        h.key_press(egui::Key::ArrowDown);
        h.run();
        let below = read(&h).0.map_or(f32::NAN, |s| s.2);
        assert!(below > top, "Down should reach a lower row: {top} -> {below}");
    }

    /// Each paragraph is aligned to its own side: an English paragraph left, an Arabic one right,
    /// whichever comes first; the caret at the end of the English one is on the left.
    #[test]
    fn each_paragraph_takes_its_own_side() {
        let mut h = harness(|ui| {
            for text in ["Hello world\nمرحبا بالعالم", "مرحبا بالعالم\nHello world"] {
                let job = LayoutJob::simple(text.to_owned(), FontId::proportional(14.0), Color32::WHITE, 300.0);
                let laid = lay_lines(ui, &job, break_lines(ui, &job, 300.0), false);
                let english = laid.row_span(if text.starts_with('H') { 0 } else { 1 }).unwrap_or(egui::Rect::NOTHING);
                let arabic = laid.row_span(if text.starts_with('H') { 1 } else { 0 }).unwrap_or(egui::Rect::NOTHING);
                assert!(english.left().abs() < 1.0, "{text:?}: English row at {english:?}");
                assert!((arabic.right() - 300.0).abs() < 1.0, "{text:?}: Arabic row at {arabic:?}");
                let end_of_english = if text.starts_with('H') { "Hello world".len() } else { text.len() };
                let x = laid.caret(ui, end_of_english).map_or(f32::NAN, |c| c.0);
                assert!((x - english.right()).abs() < 1.0, "{text:?}: caret {x} vs {english:?}");
            }
        });
        h.step();
    }

    /// A right-to-left mark alone makes text take the right-to-left path.
    #[test]
    fn a_right_to_left_mark_counts_as_right_to_left() {
        assert!(has_rtl("\u{200F}abc"));
        assert!(is_rtl_paragraph("\u{200F}abc"));
        assert!(!has_rtl("abc\u{200E}"));
    }
}
