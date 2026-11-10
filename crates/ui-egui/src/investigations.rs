//! The sidebar's Earlier investigations: what chats with no project open kept, to carry on with
//! one (#1395). The list is read on a worker through `core_investigations_list`, and opening one
//! goes through `core_investigation_open`, the same tools the CLI and MCP use.

use crate::i18n::{tr, trf};
use crate::state::{ChatItem, Screen};
use crate::theme::Tokens;
use crate::{CraftApp, icons, widgets};
use craft_anything_engine::tools::investigation_history::{LIST_TOOL, OPEN_TOOL};
use egui::{Ui, Vec2};
use serde_json::{Value, json};

/// Rows shown; older ones stay on disk and in `core_investigations_list`.
// shortcut: no paging in the sidebar; add "Show more" if people keep more than this.
const SHOWN: usize = 50;

impl CraftApp {
    /// Read the earlier investigations on a worker: drawing reads only memory.
    pub fn refresh_investigations(&mut self) {
        self.act(LIST_TOOL, json!({}), tr("Read earlier investigations"));
    }

    pub(crate) fn investigations_listed(&mut self, v: &Value) {
        self.investigations = serde_json::from_value(v["investigations"].clone()).unwrap_or_default();
    }

    pub fn open_investigation(&mut self, id: &str) {
        self.act(OPEN_TOOL, json!({ "id": id }), tr("Open the investigation"));
    }

    /// The session has the investigation now: the next message starts a chat that carries on with
    /// it, so the one going stops and its messages are cleared.
    pub(crate) fn investigation_opened(&mut self, v: &Value) {
        if let Some(mut c) = self.conversation.take() {
            c.stop();
        }
        self.state.chat.clear();
        let text = match v["question"].as_str().map(str::trim).filter(|q| !q.is_empty()) {
            Some(question) => trf("Carrying on with an earlier investigation: {question}", &[("question", question)]),
            None => tr("Carrying on with an earlier investigation."),
        };
        self.state.chat.push(ChatItem::Status(text));
        self.view_dirty = true;
        self.state.screen = if self.beginner() { Screen::Ask } else { Screen::Workbench };
        self.refresh_investigations();
    }
}

/// The section, under the pinned builds. Folded with the sidebar, it isn't shown.
pub fn section(app: &mut CraftApp, ui: &mut Ui, fold: bool) {
    if fold || app.investigations.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let shown = app.state.investigations_shown;
    let r = ui
        .horizontal(|ui| {
            ui.add_space(12.0);
            widgets::caption(ui, "Earlier investigations");
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::click());
            icons::paint(ui, rect, if shown { "chevron-down" } else { "chevron-right" }, 14.0, t.icon);
            resp
        })
        .inner;
    let what = if shown { "Hide earlier investigations" } else { "Show earlier investigations" };
    let r = r.on_hover_text(tr(what));
    widgets::named(r.clone(), what);
    if r.clicked() {
        app.state.investigations_shown = !shown;
    }
    ui.add_space(4.0);
    if shown {
        let mut open = None;
        for inv in app.investigations.iter().take(SHOWN) {
            let name = inv.question.clone().unwrap_or_else(|| tr("Investigation"));
            let evidence = inv.evidence.to_string();
            let findings = inv.findings.to_string();
            let when = crate::make::ago(inv.updated_ms);
            let r = crate::sidebar::row(ui, "history", &name, None, inv.open).on_hover_text(trf(
                "{when} · {evidence} evidence records · {findings} findings",
                &[("when", &when), ("evidence", &evidence), ("findings", &findings)],
            ));
            if r.clicked() && !inv.open {
                open = Some(inv.id.clone());
            }
        }
        if let Some(id) = open {
            app.open_investigation(&id);
        }
    }
    crate::sidebar::section_gap(ui, fold, 14.0);
}
