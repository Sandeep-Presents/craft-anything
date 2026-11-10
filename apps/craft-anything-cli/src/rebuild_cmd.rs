//! `rebuild`: the whole study → build → check workflow, headless. Questions and approvals are
//! answered on the terminal, or automatically with `--unattended` (first choice; approvals are
//! refused, so nothing outside the project happens without a person, except what `--allow`
//! granted up front).

use crate::{Cli, INTERRUPTED, make_engine};
use clap::Args;
use craft_anything_ai::rebuild::{self, RebuildRun, RunEvent, RunOptions};
use craft_anything_ai::{AgentEvent, AiHub, BackendKind};
use craft_anything_engine::inbox::{Answer, Request, RequestKind};
use craft_anything_engine::workspace::Workspace;
use craft_anything_evidence::ToolError;
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Debug, Args)]
pub struct RebuildArgs {
    /// A web address, `adb:<serial>/<package>` for an app on a phone or TV, an absolute path to
    /// an app, game or file, or `-` for none.
    pub target: String,
    /// What to make, in your words.
    pub request: String,
    /// Where the new project folder goes (default: the projects folder from settings).
    #[arg(long, conflicts_with = "project_folder")]
    pub projects_dir: Option<PathBuf>,
    /// Start the new project in this folder itself (it's added to the project list). If it
    /// already has files, --existing says what they are.
    #[arg(long, conflicts_with = "continue_in")]
    pub project_folder: Option<PathBuf>,
    /// With --project-folder that has files: `edit` (they're the app, edited in place) or
    /// `start-from` (material to start from; the app is built in app/ beside them).
    #[arg(long, requires = "project_folder", value_parser = ["edit", "start-from"])]
    pub existing: Option<String>,
    /// Continue an existing project folder; the request becomes a follow-up message.
    #[arg(long)]
    pub continue_in: Option<PathBuf>,
    /// With --continue-in: carry on where the project's last run stopped (paused, or the app or
    /// the computer stopped); the request is ignored.
    #[arg(long, requires = "continue_in")]
    pub resume: bool,
    /// The coding agent to use (an enabled backend name from `ai list`, or an installed agent
    /// such as `Claude Code`).
    #[arg(long)]
    pub backend: Option<String>,
    /// Refinement rounds after the first build (default: `build.max_rounds` from settings).
    #[arg(long)]
    pub rounds: Option<u32>,
    /// Answer questions with the first choice and refuse approvals, without asking; what
    /// `--allow` granted is still allowed.
    #[arg(long)]
    pub unattended: bool,
    /// Allow a kind of action for this project without asking, as "always for this project"
    /// would: `tool:<id>` for a craft-anything tool that runs code or changes a device
    /// (`tool:adb_keys_press`), `bash:<program>` for a command the builder runs (`bash:gradle`).
    /// Repeatable. Kept in the project (craft.json), so later runs have it too.
    #[arg(long, value_name = "KIND")]
    pub allow: Vec<String>,
    /// What to build with: an engine (unity, unreal, godot, rust) or a stack's full name.
    /// New projects only; otherwise it's chosen from the request and target.
    #[arg(long)]
    pub build_with: Option<String>,
    /// Let the builder generate pictures and 3D models for a new game (needs a fal.ai key; paid
    /// within the project's asset budget). Change it later with `project-assets-allow`.
    #[arg(long)]
    pub generate_assets: bool,
}

pub fn rebuild(cli: &Cli, a: &RebuildArgs) -> u8 {
    match run(cli, a) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("rebuild: {}", e.message);
            if let Some(r) = e.remediation {
                eprintln!("  {r}");
            }
            1
        }
    }
}

/// The kinds `--allow` takes, checked: `tool:<id>` names a tool that needs consent (one that
/// doesn't is pointless, one that doesn't exist a typo), `bash:<program>` a plain program name.
fn allowed_kinds(given: &[String]) -> Result<Vec<String>, ToolError> {
    let registry = craft_anything_engine::Registry::with_all_tools();
    let mut out = Vec::new();
    for g in given {
        let kind = g.trim();
        if let Some(id) = kind.strip_prefix("tool:") {
            match registry.get(id) {
                Some(spec) if spec.effects.needs_consent() => {}
                Some(_) => {
                    return Err(ToolError::invalid(format!("{id} never asks for consent, so --allow tool:{id} does nothing")).with_remediation("Leave it out."));
                }
                None => {
                    return Err(ToolError::invalid(format!("no tool is called {id}")).with_remediation(
                        "`craft-anything-cli commands` lists them; adb_keys_press, adb_app_install and adb_app_map are the usual ones for a device.",
                    ));
                }
            }
        } else if let Some(program) = kind.strip_prefix("bash:") {
            if program.is_empty() || program.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\') {
                return Err(ToolError::invalid(format!("--allow bash:<program> takes a plain program name, not {program:?}")));
            }
        } else {
            return Err(ToolError::invalid(format!("--allow takes tool:<id> or bash:<program>, not {kind:?}")));
        }
        if !out.contains(&kind.to_string()) {
            out.push(kind.to_string());
        }
    }
    Ok(out)
}

/// Add the agent app this run asks for, or the first one found when no enabled AI can build.
fn ensure_agent(hub: &AiHub, requested: Option<&str>) -> Result<(), ToolError> {
    let configured = hub.backends();
    // An enabled backend can build, so a run that names none needs no agent app.
    if requested.is_none() && configured.iter().any(|b| b.enabled) {
        return Ok(());
    }
    // A name that is configured is left alone (including a backend that is off): don't add a
    // second one under the same name; the run itself refuses a backend that is off.
    if requested.is_some_and(|name| configured.iter().any(|b| b.name == name)) {
        return Ok(());
    }
    // An explicit name may be an installed agent that isn't added yet: add only that one. With no
    // name, add the first agent found.
    if let Some(d) = hub.detect().into_iter().find(|d| d.suggestion.kind == BackendKind::AgentCli && requested.is_none_or(|name| d.suggestion.name == name)) {
        eprintln!("Using {} ({}).", d.suggestion.name, d.reason);
        hub.add_backend(d.suggestion, None)?;
    }
    Ok(())
}

fn run(cli: &Cli, a: &RebuildArgs) -> Result<bool, ToolError> {
    let engine = make_engine(None, cli.deep_for_run())?;
    let hub = AiHub::new(engine.clone());
    // Before the build starts, while no session lock is held (#1225).
    if let Err(e) = hub.ensure_session_id() {
        log::warn!("couldn't save the OpenCode Go session id: {e}");
    }
    if a.build_with.is_some() && a.continue_in.is_some() {
        return Err(ToolError::invalid("--build-with is for new projects; a project keeps the stack it was built with.")
            .with_remediation("Leave out --build-with, or start a new project."));
    }
    if a.generate_assets && a.continue_in.is_some() {
        return Err(ToolError::invalid("--generate-assets is for new projects.").with_remediation("Use project-assets-allow for an existing project."));
    }
    ensure_agent(&hub, a.backend.as_deref())?;
    // Refuse an unknown or switched-off name before opening or making a project: a command that
    // can't build must not leave a new folder behind.
    if let Some(name) = a.backend.as_deref() {
        rebuild::builder_backend(&hub.config(), Some(name))?;
    }
    let allow = allowed_kinds(&a.allow)?;
    let (mut ws, message) = match &a.continue_in {
        Some(dir) => (Workspace::open(&std::path::absolute(dir).map_err(|e| ToolError::io(dir.display(), e))?)?, (!a.resume).then(|| a.request.clone())),
        None => {
            use craft_anything_ai::rebuild::project::Place;
            use craft_anything_engine::workspace::ExistingFiles;
            let settings = engine.session().settings.clone();
            let existing = a.existing.as_deref().map(|e| if e == "edit" { ExistingFiles::Edit } else { ExistingFiles::StartFrom });
            let folder = a.project_folder.as_deref().map(std::path::absolute).transpose().map_err(|e| ToolError::io("--project-folder", e))?;
            let place = match (&folder, &a.projects_dir) {
                (Some(f), _) => Place::At(f, existing),
                (None, Some(d)) => Place::In(d),
                (None, None) => Place::Projects,
            };
            let ws = rebuild::project::new_project(&settings, &a.target, &a.request, place, a.build_with.as_deref(), a.generate_assets, a.backend.as_deref())?;
            if folder.is_some() {
                engine.session().remember_project(&ws.root)?;
            }
            (ws, None)
        }
    };
    if !allow.is_empty() {
        ws.set_person(|m| {
            for kind in &allow {
                if !m.allowed_actions.contains(kind) {
                    m.allowed_actions.push(kind.clone());
                }
            }
        })?;
        println!("Allowed for this project: {}", allow.join(", "));
    }
    println!("Project: {}", ws.root.display());
    // The saved builder may be off or gone; say who builds once the run has started, so a failed
    // start doesn't claim an AI that never ran.
    let replacement = rebuild::builder_replacement(&hub.config(), &ws, a.backend.as_deref());
    // A follow-up shows in the project's feed as the person typed it.
    let shown = message.clone();
    let (good_enough, max_rounds, clean_room) = {
        let s = engine.session();
        (s.settings.build.good_enough, a.rounds.unwrap_or(s.settings.build.max_rounds), s.clean_room())
    };
    let opts = RunOptions {
        backend: a.backend.clone(),
        message,
        shown,
        continuing: a.resume,
        max_rounds,
        good_enough,
        beginner: !cli.json,
        clean_room: Some(clean_room),
        ..RunOptions::default()
    };
    let mut run = RebuildRun::start(&hub, ws, opts)?;
    if let Some(replacement) = replacement {
        eprintln!("{}", rebuild::replaced_builder_notice(&replacement.saved, &replacement.used));
    }
    let mut ok = false;
    let mut line_open = false;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            run.stop();
        }
        for e in run.poll() {
            match e {
                RunEvent::Step { step, text } => {
                    end_line(&mut line_open);
                    println!("[{}] {text}", step.label());
                }
                RunEvent::Agent(AgentEvent::TextDelta { text }) => {
                    print!("{text}");
                    line_open = !text.ends_with('\n');
                }
                RunEvent::Agent(AgentEvent::ToolStarted { tool, input, .. }) => {
                    end_line(&mut line_open);
                    println!("  · {}", rebuild::activity::describe(&tool, &input, &[]).words());
                }
                RunEvent::Agent(AgentEvent::Error { message, remediation, log }) => {
                    end_line(&mut line_open);
                    println!("  ! {message}");
                    if let Some(r) = remediation {
                        println!("{r}");
                    }
                    if let Some(l) = log {
                        println!("  log: {}", l.display());
                    }
                }
                RunEvent::Agent(_) => {}
                RunEvent::Note { shown, .. } => {
                    end_line(&mut line_open);
                    println!("  + {shown}");
                }
                RunEvent::Ask(r) => {
                    end_line(&mut line_open);
                    let answer = if a.unattended { auto_answer(&r) } else { prompt_answer(&r) };
                    run.answer(&r.id, &answer)?;
                }
                RunEvent::Checked(c) => {
                    end_line(&mut line_open);
                    match c.similarity {
                        Some(s) => println!("[Check {}] {s:.0}% alike — {}", c.round, c.note),
                        None => println!("[Check {}] {}", c.round, c.note),
                    }
                }
                RunEvent::Preview(url) => println!("Preview: {url}"),
                RunEvent::Finished { ok: o, message, paused } => {
                    end_line(&mut line_open);
                    println!(
                        "{}",
                        if o {
                            format!("Done. {message}")
                        } else if paused {
                            message
                        } else {
                            format!("Stopped: {message}")
                        }
                    );
                    ok = o;
                }
            }
        }
        if !run.is_running() && run.poll().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::io::stdout().flush();
    Ok(ok)
}

fn end_line(open: &mut bool) {
    if *open {
        println!();
        *open = false;
    }
}

fn auto_answer(r: &Request) -> Answer {
    match r.kind {
        RequestKind::Question => {
            println!("? {} → {} (unattended)", r.title, r.choices.first().map(String::as_str).unwrap_or("no preference"));
            Answer {
                choice: r.choices.first().cloned(),
                text: r.choices.is_empty().then(|| "No preference; use your best judgement.".to_string()),
                ..Default::default()
            }
        }
        RequestKind::Approval => {
            println!("? {} → refused (unattended)", r.title);
            Answer { allow: Some(false), text: Some("Unattended run: actions outside the project aren't allowed.".into()), ..Default::default() }
        }
    }
}

fn read_line() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

fn prompt_answer(r: &Request) -> Answer {
    println!("\n? {}", r.title);
    if !r.detail.is_empty() {
        println!("  {}", r.detail.lines().take(12).collect::<Vec<_>>().join("\n  "));
    }
    match r.kind {
        RequestKind::Question => {
            for (i, c) in r.choices.iter().enumerate() {
                println!("  {}) {c}", i + 1);
            }
            print!("Your answer (number or text): ");
            let _ = std::io::stdout().flush();
            let line = read_line();
            match line.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| r.choices.get(i)) {
                Some(c) => Answer { choice: Some(c.clone()), ..Default::default() },
                None => Answer { text: Some(line), ..Default::default() },
            }
        }
        RequestKind::Approval => {
            print!("Allow? [y]es / [a]lways for this project / [n]o: ");
            let _ = std::io::stdout().flush();
            let line = read_line().to_lowercase();
            Answer { allow: Some(line.starts_with('y') || line.starts_with('a')), always: line.starts_with('a'), ..Default::default() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn rounds_default_to_the_settings_and_allow_takes_consent_kinds() {
        let cli = Cli::try_parse_from(["craft-anything-cli", "rebuild", "-", "A notes app"]).unwrap();
        let crate::Command::Rebuild(a) = cli.command else { panic!("not rebuild") };
        assert_eq!((a.rounds, a.allow.len()), (None, 0), "no --rounds: the settings decide");
        let cli = Cli::try_parse_from([
            "craft-anything-cli",
            "rebuild",
            "adb:192.168.1.20:5555/com.example.tv",
            "The same app",
            "--unattended",
            "--rounds",
            "8",
            "--allow",
            "tool:adb_keys_press",
            "--allow",
            "bash:gradle",
            "--allow",
            "tool:adb_keys_press",
        ])
        .unwrap();
        let crate::Command::Rebuild(a) = cli.command else { panic!("not rebuild") };
        assert_eq!(a.rounds, Some(8));
        assert_eq!(allowed_kinds(&a.allow).unwrap(), ["tool:adb_keys_press", "bash:gradle"], "each kind once");
        assert!(allowed_kinds(&["tool:adb_devices_find".into()]).is_err(), "a tool that never asks is refused");
        assert!(allowed_kinds(&["tool:no_such_tool".into()]).is_err());
        assert!(allowed_kinds(&["bash:/usr/bin/gradle".into()]).is_err(), "a program name, not a path");
        assert!(allowed_kinds(&["web_fetch".into()]).is_err());
    }
}
