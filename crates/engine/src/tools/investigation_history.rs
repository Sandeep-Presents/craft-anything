//! Earlier investigations: a chat with no project open keeps its evidence, findings and journal in
//! `<config>/investigations/session-<n>.cany`. Listing them and opening one again lets a later chat
//! carry on with that work instead of starting from nothing (#1395).

use crate::project::Project;
use crate::session::Session;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
use crate::tools::investigation_tools::Investigation;
use craft_anything_evidence::ToolError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const LIST_TOOL: &str = "core_investigations_list";
pub const OPEN_TOOL: &str = "core_investigation_open";

/// Folders looked at in the investigations folder; a listing that stops here says so.
const MAX_SCANNED: usize = 2048;
/// Evidence files read to tell whether a Clean room session may open an investigation.
const MAX_CHECKED: usize = 20_000;

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: LIST_TOOL,
            title: "Earlier investigations",
            summary: "List the investigations kept by earlier chats that had no project open, newest first: id, question, targets, how many evidence records and findings, when it last changed, and whether it's the one open now. Open one with core_investigation_open.",
            beginner: "See the investigations from your earlier chats.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            requires: Requires::Nothing,
            input_schema: empty_schema,
            run: core_investigations_list,
        },
        ToolSpec {
            id: OPEN_TOOL,
            title: "Continue an earlier investigation",
            summary: "Open an investigation from core_investigations_list by its id, so this session carries on with its evidence, unknowns, brief, plan, findings and journal. Evidence this session gathered without a project is saved to its own investigation first. Clean room can't open one that holds Deep-mode evidence. Refused while a project that isn't an investigation is open.",
            beginner: "Pick up an earlier chat's investigation where it stopped.",
            toolset: Toolset::Core,
            effects: Effects::MUTATES_SESSION.union(Effects::WRITES_FILESYSTEM),
            requires: Requires::Nothing,
            input_schema: schema_of::<OpenParams>,
            run: core_investigation_open,
        },
    ]
}

/// Where chats with no project keep their investigations.
pub fn folder(config_dir: &Path) -> PathBuf {
    config_dir.join("investigations")
}

/// One earlier investigation, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EarlierInvestigation {
    /// The folder's name, which `core_investigation_open` takes.
    pub id: String,
    /// The brief's question, when one was written.
    pub question: Option<String>,
    pub targets: Vec<String>,
    pub evidence: usize,
    pub findings: usize,
    /// When its investigation memory (or, without one, the project) last changed, in ms since 1970.
    pub updated_ms: u64,
    /// The one this session has open.
    pub open: bool,
}

/// The investigations kept under `config_dir`, newest first, and whether the scan stopped early.
pub fn list(config_dir: &Path, open: Option<&Path>) -> (Vec<EarlierInvestigation>, bool) {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder(config_dir)) else { return (found, false) };
    let mut partial = false;
    for (i, entry) in entries.flatten().enumerate() {
        if i >= MAX_SCANNED {
            partial = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_id(&name) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let root = entry.path();
        let Ok(project) = Project::open(&root) else { continue };
        let state = root.join("investigation").join("state.json");
        let investigation: Investigation = std::fs::read(&state).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let updated_ms = [state, root.join("project.toml")].iter().find_map(|p| modified_ms(p)).unwrap_or(0);
        let brief = investigation.brief.as_ref();
        found.push(EarlierInvestigation {
            id: name,
            question: brief.map(|b| b.question.trim().to_string()).filter(|q| !q.is_empty()),
            targets: brief.map(|b| b.targets.clone()).unwrap_or_default(),
            evidence: project.evidence_files().len(),
            findings: investigation.findings.len(),
            updated_ms,
            open: open.is_some_and(|o| o == root),
        });
    }
    found.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| b.id.cmp(&a.id)));
    (found, partial)
}

/// A folder name `ensure_shared_project` makes, and nothing that could leave the folder.
fn is_id(name: &str) -> bool {
    name.strip_prefix("session-").and_then(|n| n.strip_suffix(".cany")).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    u64::try_from(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()
}

fn core_investigations_list(ctx: &mut Ctx<'_>, _params: Value) -> Result<ToolOutput, ToolError> {
    let open = ctx.session.project.as_ref().map(|p| p.root.clone());
    let (investigations, partial) = list(&ctx.session.config_dir(), open.as_deref());
    Ok(ToolOutput::Value(json!({ "investigations": investigations, "partial": partial })))
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenParams {
    /// The id from `core_investigations_list` (a folder name such as `session-1700000000.cany`).
    pub id: String,
}

fn core_investigation_open(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: OpenParams = parse_params(params)?;
    open(ctx.session, &p.id)?;
    let investigation = crate::tools::investigation_tools::cached(ctx.session);
    Ok(ToolOutput::Value(json!({
        "opened": p.id,
        "question": investigation.brief.map(|b| b.question),
        "evidence_loaded": ctx.session.ledger_len(),
        "findings": investigation.findings.len(),
    })))
}

/// Make the investigation `id` this session's project. Nothing the session holds is lost: work
/// gathered without a project is saved to an investigation of its own first.
pub fn open(session: &mut Session, id: &str) -> Result<(), ToolError> {
    let earlier = || ToolError::invalid(format!("There's no earlier investigation called {id}.")).with_remediation(format!("Pick one from {LIST_TOOL}."));
    if !is_id(id) {
        return Err(earlier());
    }
    let investigations = folder(&session.config_dir());
    let root = investigations.join(id);
    let project = Project::open(&root).map_err(|_| earlier())?;
    if let Some(current) = &session.project {
        if current.root == root {
            return Ok(());
        }
        if current.root.parent() != Some(investigations.as_path()) {
            return Err(ToolError::invalid("A project is open, so its work isn't an investigation to set aside.")
                .with_remediation("Close the project first, then open the earlier investigation."));
        }
    }
    if session.clean_room() {
        check_clean_room(&project)?;
    }
    if session.project.is_none() && (session.ledger_len() > 0 || session.unknowns().next().is_some()) {
        session.ensure_shared_project()?;
    }
    session.clear_ledger();
    session.project = None;
    session.open_project(project)
}

/// Clean room never sees what Deep mode read, so an investigation that holds any of it stays shut.
fn check_clean_room(project: &Project) -> Result<(), ToolError> {
    let deep = || {
        ToolError::invalid("This investigation holds evidence read in Deep mode, which Clean room doesn't open.")
            .with_remediation("Turn on Deep mode to continue it, or start a new chat.")
    };
    let files = project.evidence_files();
    if files.len() > MAX_CHECKED {
        return Err(deep());
    }
    for f in &files {
        match project.load_evidence(&f.id) {
            Ok(ev) if ev.environment.clean_room => {}
            Ok(_) => return Err(deep()),
            Err(e) => log::warn!("skipping project evidence {}: {}", f.id, e.message),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::project::Project;
    use crate::session::Session;
    use crate::{CallOptions, Caller, Engine};
    use craft_anything_evidence::{Environment, Evidence, EvidenceBuilder, Subject};
    use serde_json::{Value, json};

    fn call(e: &Engine, id: &str, p: Value) -> Result<Value, craft_anything_evidence::ToolError> {
        e.call(id, p, CallOptions::new(Caller::Agent)).map(|o| o.to_json()["result"].clone())
    }

    fn evidence(name: &str, clean_room: bool) -> Evidence {
        EvidenceBuilder::new(name, Subject::default(), Environment::builtin(clean_room)).result(json!(name)).summary(name).build().unwrap()
    }

    /// One chat with no project: it records what it found, then its investigation is kept.
    fn earlier(engine: &Engine, question: &str, clean_room: bool) -> String {
        engine.session().record(evidence(question, clean_room)).unwrap();
        call(engine, "core_brief_write", json!({ "question": question, "targets": ["app.dll"] })).unwrap();
        let root = engine.session().ensure_shared_project().unwrap();
        let mut s = engine.session();
        s.clear_ledger();
        s.project = None;
        root.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn earlier_investigations_are_listed_newest_first_and_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Session::isolated(dir.path()));
        let first = earlier(&engine, "What does it send?", true);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = earlier(&engine, "Where are the keys kept?", true);
        // Not a kept investigation: other folders and names are left out.
        std::fs::create_dir_all(super::folder(&engine.session().config_dir()).join("notes")).unwrap();

        let listed = call(&engine, super::LIST_TOOL, json!({})).unwrap();
        let ids: Vec<&str> = listed["investigations"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, [second.as_str(), first.as_str()], "newest first");
        let top = &listed["investigations"][0];
        assert_eq!(top["question"], "Where are the keys kept?");
        assert_eq!(top["targets"], json!(["app.dll"]));
        assert_eq!(top["evidence"], 1);
        assert_eq!(top["open"], false);

        // A chat that found something, with no project yet: that work is kept before switching.
        engine.session().record(evidence("unsaved", true)).unwrap();
        let opened = call(&engine, super::OPEN_TOOL, json!({ "id": first })).unwrap();
        assert_eq!(opened["question"], "What does it send?");
        assert_eq!(opened["evidence_loaded"], 1, "only the reopened investigation's evidence");
        assert_eq!(engine.session().project.as_ref().unwrap().root.file_name().unwrap().to_string_lossy(), first);
        let listed = call(&engine, super::LIST_TOOL, json!({})).unwrap()["investigations"].clone();
        assert_eq!(listed.as_array().unwrap().len(), 3, "the unsaved work became an investigation of its own");
        assert!(listed.as_array().unwrap().iter().any(|i| i["id"] == first.as_str() && i["open"] == true));

        // From one investigation to another.
        call(&engine, super::OPEN_TOOL, json!({ "id": second })).unwrap();
        assert_eq!(super::super::investigation_tools::cached(&engine.session()).brief.unwrap().question, "Where are the keys kept?");
    }

    #[test]
    fn only_kept_investigations_open() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Session::isolated(dir.path()));
        let id = earlier(&engine, "Q", true);
        for bad in ["../session-1.cany", "session-.cany", "session-1.cany/..", "notes", "session-999.cany", ""] {
            let error = call(&engine, super::OPEN_TOOL, json!({ "id": bad })).unwrap_err();
            assert!(error.message.contains("no earlier investigation"), "{bad}: {}", error.message);
        }
        // A project that isn't an investigation isn't set aside for one.
        let project = Project::create(&dir.path().join("work.cany"), "Work", Default::default()).unwrap();
        engine.session().open_project(project).unwrap();
        let error = call(&engine, super::OPEN_TOOL, json!({ "id": id })).unwrap_err();
        assert!(error.message.contains("A project is open"), "{}", error.message);
    }

    #[test]
    fn clean_room_does_not_open_what_deep_mode_read() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Session::isolated(dir.path()));
        let id = earlier(&engine, "Q", false);
        assert!(engine.session().clean_room());
        let error = call(&engine, super::OPEN_TOOL, json!({ "id": id })).unwrap_err();
        assert!(error.message.contains("Deep mode"), "{}", error.message);
        assert!(engine.session().project.is_none(), "nothing changed");
        engine.session().settings.deep_mode = true;
        call(&engine, super::OPEN_TOOL, json!({ "id": id })).unwrap();
    }
}
