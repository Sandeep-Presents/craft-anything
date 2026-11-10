//! pi (`pi`) as a coding agent: one message in on stdin, its JSONL event stream out.
//!
//! `pi -p --mode json` runs without the terminal UI and writes one JSON object per line: a session
//! header, `message_update` deltas carrying the text, `tool_execution_start`/`tool_execution_end`
//! for each call, `message_end` with the turn's usage, and `agent_settled` at the end. The session
//! header's `id` continues the same conversation with `--session-id`, creating it the first time.
//!
//! pi has no flag for a tool server the way other agents do: servers come from a project's
//! `mcp.json` (which needs the project trusted) and from extensions. Our tool server therefore
//! arrives as a one-function extension this run loads with `-e`, registering it with
//! `exposure: "direct"` so its tools are declared to the model instead of being kept behind
//! `codemode`. The file is written into a folder of the run's own, owner-only on Unix, so nothing
//! another user could prepare in a shared temporary folder is ever loaded through.
//!
//! `--tools` with an `mcp__` entry filters MCP tools as well as built-in ones (pi's own documented
//! behaviour), so a builder keeps our tools plus reading only: it has no `edit`, `write` or `bash`
//! of its own, and every file change and command goes through the tools whose effects, evidence
//! and consent Craft Anything records. `codemode` and `tool_search` are left out on purpose:
//! `codemode` calls a server's tools by name whatever the active tool set says, so leaving it in
//! would reach the servers this run isn't meant to use.
//!
//! pi has no `--permission-prompt-tool`, so the same extension holds the bridge: its `tool_call`
//! handler asks Craft Anything's own `core_action_approve` about every tool that changes something
//! (pi's `bash`, `powershell`, `write`, `edit`) and blocks the call when the policy refuses or the
//! person says no. Reads and our own tools pass straight through: ours carry their own consent.
//! With that in place a Deep build may use pi's own editor and shell, as a Claude Code build does.
//!
//! `sh`-style quoting is not involved anywhere: the extension is JSON, and the message never
//! enters argv (Windows refuses long command lines, and its limit is far below a build's brief).

use super::*;

use std::sync::atomic::{AtomicU64, Ordering};

/// The name our extension registers the server under. Not `craft-anything`, which a person's own
/// `mcp.json` entry may use: pi refuses two clients with one name, and a `mcp.json` entry would
/// win over the extension's.
const SERVER: &str = "craft-anything-run";

/// The name our tools reach the model under: pi turns every character other than letters, digits
/// and `_` into `_`, so `craft-anything-run` becomes `craft_anything_run`.
const TOOL_PREFIX: &str = "mcp__craft_anything_run__";

/// The built-in tools a builder keeps beside ours: reading, and in Deep mode the editor and shell
/// the extension's approval bridge gates. pi has no `TodoWrite`.
const READ_TOOLS: &[&str] = &["read", "grep", "find", "ls"];
const CHANGING_TOOLS: &[&str] = &["edit", "write", "bash", "powershell"];

/// The extension's file name inside a run's folder.
const EXTENSION_FILE: &str = "craft-anything-tools.js";

/// The file a preflight writes and removes to prove the folder takes an extension.
const PROBE_FILE: &str = "probe.js";

/// pi run as a coding agent.
pub struct PiCli;

impl AgentAdapter for PiCli {
    fn ids(&self) -> &'static [&'static str] {
        &["pi"]
    }

    /// The message goes in on stdin: `pi -p` reads it there when no prompt is given, which keeps a
    /// build's brief out of argv and away from Windows' command-line limit.
    fn prompt_on_stdin(&self) -> bool {
        true
    }

    fn streams_json(&self) -> bool {
        true
    }

    fn answers_questions(&self) -> bool {
        true
    }

    fn resumes(&self) -> bool {
        true
    }

    /// `--help` lists `--mode` and `--session-id`; without them the plain form would boot the
    /// terminal UI in the middle of a build, so a failed check keeps this adapter.
    fn required_flags(&self) -> (&'static [&'static str], &'static [&'static str]) {
        (&["--help"], &["--mode", "--session-id"])
    }

    fn plain_fallback(&self) -> bool {
        false
    }

    fn check_tool_server(&self, stop: &CancelToken) -> Result<(), ToolError> {
        cli_can_start(craft_anything_platform::self_cli_path().as_deref(), stop)?;
        // The folder and the write a run uses, once, before the build starts: a temporary folder
        // that can't take the extension stops the build here with a fix, not part-way through one.
        let dir = extension_dir()?;
        let written = craft_anything_platform::write_atomic_private(&dir.join(PROBE_FILE), b"// probe\n");
        let _ = std::fs::remove_dir_all(&dir);
        written
    }

    fn model_in_use(&self, options: Option<&BackendOptions>) -> Option<String> {
        Some(options.and_then(|o| o.cli_model.clone()).filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "pi's own default".into()))
    }

    /// Sign-in belongs to the provider pi was set up with, and checking it needs a provider or a
    /// model name the adapter isn't given. The run itself reports what the provider refused.
    fn sign_in_fix(&self) -> Option<String> {
        Some("Sign in the way pi does: run `pi` once in a terminal, sign in to the provider its default model uses, then start the build again.".into())
    }

    /// pi keeps each session as `sessions/--<folder>--/<timestamp>_<id>.jsonl` under its own
    /// folder, so a project can keep a copy that travels with it.
    fn session_file(&self, id: &str) -> Option<std::path::PathBuf> {
        agent_dir().and_then(|dir| session_file_in(&dir, id))
    }

    fn handoff(&self, saved: &str, max: usize) -> Option<String> {
        pi_handoff(saved, max)
    }

    fn restore_session(&self, id: &str, saved: &[u8], cwd: &std::path::Path) -> Result<bool, ToolError> {
        let dir = agent_dir().ok_or_else(|| ToolError::new(ErrorCode::BadSettings, "Couldn't find pi's folder on this computer."))?;
        restore_session_in(&dir, id, saved, cwd)
    }

    fn args(&self, cx: &ArgsCx) -> Vec<String> {
        let mut a = cx.options.args.clone();
        a.extend(["-p".into(), "--mode".into(), "json".into(), "--append-system-prompt".into(), cx.preamble.to_string()]);
        if let Some(model) = cx.options.cli_model.clone().filter(|m| !m.trim().is_empty()) {
            a.extend(["--model".into(), model]);
        }
        if let Some(effort) = cx.options.cli_thinking {
            a.extend(["--thinking".into(), effort.as_str().into()]);
        }
        if cx.question {
            // A question only reads, and a fresh session leaves the builder's own conversation
            // untouched: no `--session-id`, and no tool server.
            if cx.reads_checked {
                a.push("--no-tools".into());
            } else {
                a.extend(["--tools".into(), READ_TOOLS.join(",")]);
            }
            return a;
        }
        a.extend(["-e".into(), extension_file(&builder_server(cx.mcp))]);
        let mut tools: Vec<String> = Vec::new();
        // In Clean room the original may only be read through our tools, so no file tools of the
        // agent's own: the same rule its other adapters hold it to. In Deep mode a builder edits
        // and runs with pi's own tools, and the extension's `tool_call` bridge asks Craft Anything
        // about each of those calls.
        if !cx.reads_checked {
            tools.extend(READ_TOOLS.iter().map(|t| (*t).to_string()));
            tools.extend(CHANGING_TOOLS.iter().map(|t| (*t).to_string()));
        }
        tools.push(format!("{TOOL_PREFIX}*"));
        a.extend(["--tools".into(), tools.join(",")]);
        if let Some(resume) = cx.resume {
            a.extend(["--session-id".into(), resume.to_string()]);
        }
        a
    }

    /// Each JSON line: the session header's id, text deltas, tool calls and their results, the
    /// turn's usage, and a retry that ended in failure as an error in plain words.
    fn parse(&self, p: &mut StreamParser, v: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        match s("type") {
            "session" => {
                if !s("id").is_empty() {
                    p.session_id = Some(s("id").to_string());
                    p.run_session = p.session_id.clone();
                }
            }
            "message_update" => {
                if let Some(update) = v.get("assistantMessageEvent")
                    && update.get("type").and_then(Value::as_str) == Some("text_delta")
                    && let Some(delta) = update.get("delta").and_then(Value::as_str)
                {
                    out.push(AgentEvent::TextDelta { text: delta.to_string() });
                }
            }
            "message_end" => {
                let Some(message) = v.get("message") else { return out };
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    return out;
                }
                p.saw_result = true;
                if let Some(usage) = message.get("usage") {
                    account(p, usage);
                }
                if let Some(why) = message.get("errorMessage").and_then(Value::as_str) {
                    out.push(failed(p, why));
                }
            }
            "tool_execution_start" => {
                let id = s("toolCallId").to_string();
                // The run's own server is named so it can't clash with a person's entry, which
                // makes its tools longer than the ids the rest of Craft Anything matches on.
                let name = s("toolName");
                let name = name.strip_prefix(TOOL_PREFIX).unwrap_or(name);
                out.push(p.started(&id, name, v.get("args").cloned().unwrap_or(Value::Null)));
            }
            "tool_execution_end" => {
                let id = s("toolCallId").to_string();
                let ok = !v.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let text = result_text(v.get("result"));
                out.push(p.finished(&id, ok, &text));
            }
            "auto_retry_end" if v.get("success").and_then(Value::as_bool) == Some(false) => {
                let why = s("finalError");
                let why = if why.is_empty() { "the provider stopped answering".to_string() } else { why.to_string() };
                out.push(failed(p, &why));
            }
            _ => {}
        }
        out
    }
}

/// Add one assistant message's usage to the turn's totals. pi counts cached reads separately, and
/// its `cost` is what an API at list prices would have charged, not what a plan was billed.
fn account(p: &mut StreamParser, usage: &Value) {
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    p.input_tokens = p.input_tokens.saturating_add(n("input")).saturating_add(n("cacheRead"));
    p.output_tokens = p.output_tokens.saturating_add(n("output"));
    if let Some(total) = usage.get("cost").and_then(|c| c.get("total")).and_then(Value::as_f64)
        && total > 0.0
    {
        p.cost_usd = Some(p.cost_usd.unwrap_or(0.0) + total);
    }
}

/// A run that ended in failure, as a typed failure and a message with the fix.
fn failed(p: &mut StreamParser, why: &str) -> AgentEvent {
    let kind = classify(why);
    p.failure.get_or_insert(TurnFailure::new(kind));
    AgentEvent::Error { message: format!("pi stopped before it answered: {}", why.trim()), remediation: Some(remediation(kind).to_string()), log: None }
}

/// The plain-words failure behind a provider's message, for the stop rules and the retry policy.
fn classify(why: &str) -> FailureKind {
    let why = why.to_ascii_lowercase();
    if why.contains("401") || why.contains("unauthor") || why.contains("api key") || why.contains("invalid key") || why.contains("not signed in") {
        FailureKind::Authentication
    } else if why.contains("402") || why.contains("quota") || why.contains("credit") || why.contains("billing") || why.contains("insufficient") {
        FailureKind::Billing
    } else if why.contains("429") || why.contains("rate limit") || why.contains("too many requests") {
        FailureKind::RateLimited
    } else if why.contains("timeout") || why.contains("timed out") || why.contains("overload") || why.contains("529") || why.contains("503") {
        FailureKind::Transient
    } else {
        FailureKind::Unknown
    }
}

/// What to do about a failure, in the words a person can act on.
fn remediation(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Authentication => "Sign in to the provider pi uses: run `pi` once in a terminal, sign in, then start the build again.",
        FailureKind::Billing => "Check the account's plan, credits and spending limit in the provider pi uses, then start the build again.",
        FailureKind::RateLimited => "Wait a minute, or choose another model in Settings, then start the build again.",
        FailureKind::Transient => "This looks temporary: the build's next attempt may get through. If it keeps happening, choose another model in Settings.",
        FailureKind::MissingProgram | FailureKind::ToolsUnavailable | FailureKind::Configuration | FailureKind::Unknown => {
            "Run `pi` once in this project's folder to see the same answer without Craft Anything, then start the build again."
        }
    }
}

/// The text of a tool result: pi's blocks, joined.
fn result_text(result: Option<&Value>) -> String {
    let Some(result) = result else { return String::new() };
    match result.get("content").and_then(Value::as_array) {
        Some(blocks) => blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        None => result.as_str().unwrap_or_default().to_string(),
    }
}

/// Our server as a builder gets it: listing every tool, because this builder has no editor or
/// shell of its own and the build tools (`build_file_write`, `build_command_run`) are the only way
/// it can change the project. Clean room still refuses what it may not read: listing a tool never
/// makes it available.
fn builder_server(mcp: &McpCommand) -> McpCommand {
    let mut server = mcp.clone();
    if !server.args.iter().any(|a| a == "--all-tools") {
        server.args.push("--all-tools".into());
    }
    server
}

/// pi's own folder: `PI_CODING_AGENT_DIR`, else `~/.pi/agent`. A folder chosen for the backend in
/// Settings is not applied: pi reads that variable itself, and a second place to look would put
/// sessions where pi does not.
fn agent_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("PI_CODING_AGENT_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => Some(std::path::PathBuf::from(dir)),
        None => dirs::home_dir().map(|home| home.join(".pi").join("agent")),
    }
}

/// An id pi would use: a UUID, or whatever `--session-id` was given. Anything else (a hand-edited
/// project file) names no session.
fn session_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The folder pi gives a working folder: the leading separator removed and `/`, `\` and `:` read
/// as `-`, between two `--` (`/w/P` is `--w-P--`, as pi's own session files are named).
fn session_folder(cwd: &std::path::Path) -> String {
    let text = cwd.to_string_lossy();
    let text = text.strip_prefix('/').unwrap_or(&text);
    format!("--{}--", text.replace(['/', '\\', ':'], "-"))
}

/// Folders under pi's `sessions/` looked through for one session, so a big history stays bounded.
const SESSION_FOLDERS: usize = 10_000;

/// The file pi keeps session `id` in under `dir`, if this computer has it: the newest one, since a
/// run that didn't find a put-back copy starts a second file under the same id.
fn session_file_in(dir: &std::path::Path, id: &str) -> Option<std::path::PathBuf> {
    if !session_id_ok(id) {
        return None;
    }
    let suffix = format!("_{id}.jsonl");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for folder in std::fs::read_dir(dir.join("sessions")).ok()?.flatten().take(SESSION_FOLDERS) {
        let Ok(files) = std::fs::read_dir(folder.path()) else { continue };
        for path in files.flatten().map(|f| f.path()) {
            let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&suffix));
            if !named || !path.is_file() {
                continue;
            }
            let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else { continue };
            if newest.as_ref().is_none_or(|(at, _)| modified > *at) {
                newest = Some((modified, path));
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// Where a kept copy put back for `cwd` belongs: pi's folder for that working folder, named after
/// the session's own timestamp so pi reads it as one of its own.
fn kept_session_path(dir: &std::path::Path, id: &str, stamp: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    dir.join("sessions").join(session_folder(cwd)).join(format!("{stamp}_{id}.jsonl"))
}

/// A stamp for a kept session that names no timestamp of its own.
const KEPT_STAMP: &str = "1970-01-01T00-00-00-000Z";

/// Put a kept copy of session `id` back where pi looks for it in `cwd`. `Ok(false)`: it's here
/// already. A copy of another session is refused rather than restored.
fn restore_session_in(dir: &std::path::Path, id: &str, saved: &[u8], cwd: &std::path::Path) -> Result<bool, ToolError> {
    if !session_id_ok(id) {
        return Err(ToolError::invalid("The project names its builder's session with an id pi doesn't use."));
    }
    if session_file_in(dir, id).is_some() {
        return Ok(false);
    }
    let header = saved.split(|b| *b == b'\n').next().and_then(|line| serde_json::from_slice::<Value>(line).ok());
    let Some(header) = header.filter(|h| h.get("id").and_then(Value::as_str) == Some(id)) else {
        return Err(ToolError::invalid("The conversation kept in the project is of another session."));
    };
    let stamp = header
        .get("timestamp")
        .and_then(Value::as_str)
        .map(|t| t.replace([':', '.'], "-"))
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == 'T'))
        .unwrap_or_else(|| KEPT_STAMP.to_string());
    // `/tmp` is a link to `/private/tmp` on macOS, and `/var` to `/private/var`: pi's own working
    // folder is the real one, so the copy has to land there or the run won't find it.
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let path = kept_session_path(dir, id, &stamp, &cwd);
    craft_anything_platform::fs::write_atomic_private(&path, saved)?;
    Ok(true)
}

/// What a new session needs to know of an earlier one: the newest exchanges in the kept copy of a
/// session, each clipped, keeping at most `max` characters. pi's entries are a tree (`id`/
/// `parentId`); the newest are the tail of the file, which is the branch that was last written.
fn pi_handoff(saved: &str, max: usize) -> Option<String> {
    let clip = |text: &str| {
        let text = text.trim();
        if text.chars().count() > HANDOFF_ENTRY { format!("{}…", text.chars().take(HANDOFF_ENTRY).collect::<String>()) } else { text.to_string() }
    };
    let texts = |content: Option<&Value>| -> String {
        content
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut entries = Vec::new();
    for v in saved.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if v.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(message) = v.get("message") else { continue };
        let content = message.get("content");
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let text = content.and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| texts(content));
                if !text.trim().is_empty() {
                    entries.push(format!("Craft Anything asked you: {}", clip(&text)));
                }
            }
            Some("assistant") => {
                let text = texts(content);
                if !text.trim().is_empty() {
                    entries.push(format!("You answered: {}", clip(&text)));
                }
            }
            _ => {}
        }
    }
    let mut kept = Vec::new();
    let mut used = 0usize;
    for entry in entries.iter().rev() {
        let n = entry.chars().count().saturating_add(2);
        if used.saturating_add(n) > max {
            break;
        }
        used = used.saturating_add(n);
        kept.push(entry.as_str());
    }
    kept.reverse();
    (!kept.is_empty()).then(|| kept.join("\n\n"))
}

/// The extension file this run loads, or a path that isn't there when the folder can't take one,
/// so the run fails at startup instead of building without the tool server it was promised
/// ([`check_tool_server`](AgentAdapter::check_tool_server) reports the everyday causes first).
fn extension_file(mcp: &McpCommand) -> String {
    let Ok(dir) = extension_dir() else { return missing_extension() };
    let path = dir.join(EXTENSION_FILE);
    match craft_anything_platform::write_atomic_private(&path, extension_js(mcp).as_bytes()) {
        Ok(()) => path.display().to_string(),
        Err(e) => {
            log::warn!("pi's tool-server extension couldn't be written: {}", e.message);
            missing_extension()
        }
    }
}

/// A path no run wrote anything to.
fn missing_extension() -> String {
    std::env::temp_dir().join(format!("craft-anything-pi-{}-missing.js", std::process::id())).display().to_string()
}

/// A folder of this process's own for one run, in the system temporary folder: two builds at once
/// never share one, and because it's owner-only nothing another user could prepare in a shared
/// temporary folder is ever loaded through.
fn extension_dir() -> Result<std::path::PathBuf, ToolError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("craft-anything-pi-{}-{n}", std::process::id()));
    // `create_dir` refuses a path that is already there, so a folder prepared beforehand is never
    // used (`create_dir_all` would accept one).
    super::dsh::private_dir().create(&dir).map_err(|e| {
        ToolError::io(dir.display(), e).with_remediation(
            "Free space on the drive that holds the temporary folder, or point TMP at one that can be written, then start the build again.".to_string(),
        )
    })?;
    Ok(dir)
}

/// The arguments that ask Craft Anything about one action: `core-action-approve` with the run's
/// project and mode. `--inbox`, `--all-tools` and `--target` aren't options of that tool, and the
/// inbox is read from the project.
fn approve_args(mcp: &McpCommand) -> Vec<String> {
    let mut args = vec!["core-action-approve".to_string()];
    let mut rest = mcp.args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--project" => {
                args.push(arg.clone());
                if let Some(value) = rest.next() {
                    args.push(value.clone());
                }
            }
            "--deep" | "--clean-room" => args.push(arg.clone()),
            _ => {}
        }
    }
    args
}

/// Our tool server as the extension this run loads: the registration, declared directly so its
/// tools are in the model's list, and the approval bridge for pi's own changing tools. Under JSON
/// quoting rather than shell quoting.
fn extension_js(mcp: &McpCommand) -> String {
    let server = serde_json::Value::String(SERVER.to_string());
    let config = serde_json::json!({
        "command": mcp.command,
        "args": mcp.args,
        "exposure": "direct",
        "description": "Craft Anything's tools for this run",
    });
    let cli = serde_json::Value::String(mcp.command.clone());
    let approve = serde_json::Value::Array(approve_args(mcp).iter().map(|a| serde_json::Value::String(a.clone())).collect());
    format!(
        "// Craft Anything's tool server and approval bridge for this run. Generated; safe to delete.\n\
         import {{ execFile }} from \"node:child_process\";\n\
         import {{ promisify }} from \"node:util\";\n\
         const craft = promisify(execFile);\n\
         const CLI = {cli};\n\
         const APPROVE = {approve};\n\
         // pi's own changing tools, in the shape Craft Anything's policy reads (Claude Code's).\n\
         const SHAPE = {{\n  \
           bash: (i) => [\"Bash\", {{ command: i.command }}],\n  \
           powershell: (i) => [\"PowerShell\", {{ command: i.command }}],\n  \
           write: (i) => [\"Write\", {{ file_path: i.path, content: i.content }}],\n  \
           edit: (i) => [\"Edit\", {{ file_path: i.path, old_string: i.oldText, new_string: i.newText }}],\n\
         }};\n\
         export default function (pi) {{\n  \
           pi.registerMcpServer({server}, {config});\n  \
           // Nothing changes without asking Craft Anything first. Readings and our own tools pass.\n  \
           pi.on(\"tool_call\", async (event) => {{\n    \
             const name = String(event.toolName ?? \"\");\n    \
             if (name.startsWith(\"mcp__\")) return;\n    \
             const shape = SHAPE[name];\n    \
             if (!shape) return;\n    \
             const [tool, input] = shape(event.input ?? {{}});\n    \
             let verdict;\n    \
             try {{\n      \
               const {{ stdout }} = await craft(CLI, [...APPROVE, \"--tool-name\", tool, \"--input\", JSON.stringify(input)], {{ maxBuffer: 1 << 20 }});\n      \
               verdict = JSON.parse(stdout)?.result;\n    \
             }} catch (e) {{\n      \
               return {{ block: true, reason: \"Craft Anything couldn't check this action, so it wasn't done: \" + (e?.message ?? e) }};\n    \
             }}\n    \
             if (verdict?.behavior === \"allow\") return;\n    \
             return {{ block: true, reason: String(verdict?.message ?? \"Craft Anything refused this action.\") }};\n  \
           }});\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Effort;

    /// A tool server command for a run's project.
    fn mcp() -> McpCommand {
        McpCommand {
            command: "/Applications/Craft Anything.app/Contents/MacOS/craft-anything-cli".into(),
            args: vec!["mcp".into(), "--project".into(), "/w/P/.cany".into(), "--deep".into()],
            notes: None,
        }
    }

    /// The arguments for one message.
    fn args(options: &BackendOptions, builder: bool, question: bool, reads_checked: bool, resume: Option<&str>) -> (Vec<String>, McpCommand) {
        let mcp = mcp();
        let cx = ArgsCx { options, prompt: "make a game", preamble: "PRE ", mcp: &mcp, resume, builder, reads_checked, question };
        (PiCli.args(&cx), mcp.clone())
    }

    /// The value after `flag`.
    fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let i = args.iter().position(|a| a == flag).expect("the flag is there");
        args.get(i + 1).map(String::as_str).expect("a value after it")
    }

    /// The message never enters argv, and the stream is the JSON one.
    #[test]
    fn the_message_stays_out_of_argv_and_the_stream_is_json() {
        let (a, _) = args(&BackendOptions::default(), true, false, false, None);
        assert_eq!(a[0..4], ["-p", "--mode", "json", "--append-system-prompt"], "{a:?}");
        assert_eq!(after(&a, "--append-system-prompt"), "PRE ");
        assert!(PiCli.prompt_on_stdin(), "the message must go in on stdin");
        assert!(!a.iter().any(|arg| arg.contains("make a game")), "{a:?}");
        assert!(PiCli.streams_json());
    }

    /// A resumed session continues the conversation; a question starts a fresh one, so it leaves
    /// the builder's conversation as it was.
    #[test]
    fn a_resumed_build_continues_the_session_and_a_question_does_not() {
        let (a, _) = args(&BackendOptions::default(), true, false, false, Some("s-1"));
        assert_eq!(after(&a, "--session-id"), "s-1");
        let (q, _) = args(&BackendOptions::default(), false, true, false, Some("s-1"));
        assert!(!q.iter().any(|x| x == "--session-id"), "a question starts fresh: {q:?}");
    }

    /// The model and thinking effort a person chose reach the CLI.
    #[test]
    fn a_chosen_model_and_effort_reach_the_cli() {
        let options = BackendOptions { cli_model: Some("magpie/foxcode/claude-sonnet-5-5".into()), cli_thinking: Some(Effort::High), ..Default::default() };
        let (a, _) = args(&options, true, false, false, None);
        assert_eq!(after(&a, "--model"), "magpie/foxcode/claude-sonnet-5-5");
        assert_eq!(after(&a, "--thinking"), "high");
    }

    /// The run's own tool server, as an extension in a folder only its owner can read.
    #[test]
    fn the_tool_server_arrives_as_an_extension_in_the_runs_own_folder() {
        let (a, mcp) = args(&BackendOptions::default(), true, false, false, None);
        let path = std::path::PathBuf::from(after(&a, "-e"));
        assert!(path.exists(), "{path:?}");
        let written = std::fs::read_to_string(&path).expect("read the extension");
        assert!(written.contains("registerMcpServer"), "{written}");
        assert!(written.contains(SERVER), "the run's own name: {written}");
        assert!(written.contains("\"exposure\":\"direct\""), "declared to the model: {written}");
        assert!(written.contains(&mcp.command), "{written}");
        assert!(written.contains("--project"), "the run's own arguments: {written}");
        // A folder of the run's own, so nothing another user prepared is loaded through it.
        let folder = path.parent().expect("a folder of its own").to_path_buf();
        let own = format!("craft-anything-pi-{}-", std::process::id());
        assert!(folder.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&own)), "{folder:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let bits = |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(bits(&folder), 0o700, "folder {:o}", bits(&folder));
            assert_eq!(bits(&path), 0o600, "extension {:o}", bits(&path));
        }
        // Two runs never share one.
        let (b, _) = args(&BackendOptions::default(), true, false, false, None);
        let other = std::path::PathBuf::from(after(&b, "-e")).parent().expect("a folder").to_path_buf();
        assert_ne!(folder, other);
        for dir in [folder, other] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// A Deep builder edits and runs with pi's own tools (the bridge gates them), and `codemode` is
    /// out because it could call another server's tools by name.
    #[test]
    fn a_deep_builder_may_edit_and_run_but_not_reach_other_servers() {
        let (a, _) = args(&BackendOptions::default(), true, false, false, None);
        let tools: Vec<&str> = after(&a, "--tools").split(',').collect();
        for own in ["codemode", "tool_search"] {
            assert!(!tools.contains(&own), "{own} must not be there: {tools:?}");
        }
        for kept in READ_TOOLS.iter().chain(CHANGING_TOOLS) {
            assert!(tools.contains(kept), "{kept} belongs to a Deep builder: {tools:?}");
        }
        assert!(tools.iter().any(|t| t.starts_with(TOOL_PREFIX)), "our tools: {tools:?}");
    }

    /// Every changing tool pi offers goes through Craft Anything's own approval first, from the
    /// same generated file, so nothing outside the repository is installed into pi.
    #[test]
    fn the_extension_holds_the_approval_bridge() {
        let (a, mcp) = args(&BackendOptions::default(), true, false, false, None);
        let path = std::path::PathBuf::from(after(&a, "-e"));
        let written = std::fs::read_to_string(&path).expect("read the extension");
        for shape in ["bash:", "powershell:", "write:", "edit:"] {
            assert!(written.contains(shape), "{shape} is gated: {written}");
        }
        assert!(written.contains("pi.on(\"tool_call\""), "{written}");
        assert!(written.contains("\"core-action-approve\""), "{written}");
        assert!(written.contains(&format!("\"{}\"", mcp.args[2])), "the run's project is passed: {written}");
        let bridge = written.lines().find(|l| l.trim_start().starts_with("const APPROVE")).expect("the approval command");
        assert!(bridge.contains("\"core-action-approve\""), "{bridge}");
        assert!(!bridge.contains("--all-tools") && !bridge.contains("--inbox"), "only the approval tool's own options: {bridge}");
        assert!(bridge.contains("--deep") || bridge.contains("--clean-room"), "the run's mode: {bridge}");
        let folder = path.parent().expect("a folder").to_path_buf();
        let _ = std::fs::remove_dir_all(folder);
    }

    /// A builder has no editor of its own, so its tool server lists the build tools.
    #[test]
    fn a_builders_server_lists_every_tool() {
        let (a, _) = args(&BackendOptions::default(), true, false, false, None);
        let path = std::path::PathBuf::from(after(&a, "-e"));
        let written = std::fs::read_to_string(&path).expect("read the extension");
        assert!(written.contains("--all-tools"), "{written}");
        let folder = path.parent().expect("a folder").to_path_buf();
        let _ = std::fs::remove_dir_all(folder);
        // A question has no server, and an already-complete list isn't doubled.
        let complete = McpCommand { command: "cli".into(), args: vec!["mcp".into(), "--all-tools".into()], notes: None };
        let server = builder_server(&complete);
        assert_eq!(server.args.iter().filter(|a| *a == "--all-tools").count(), 1, "{:?}", server.args);
    }

    /// In Clean room nothing of the original is read through the agent's own file tools.
    #[test]
    fn a_clean_room_builder_gets_no_file_tools() {
        let (a, _) = args(&BackendOptions::default(), true, false, true, None);
        assert_eq!(after(&a, "--tools"), format!("{TOOL_PREFIX}*"));
        let (q, _) = args(&BackendOptions::default(), false, true, true, None);
        assert!(q.iter().any(|x| x == "--no-tools"), "a Clean room question gets no tools: {q:?}");
        assert!(!q.iter().any(|x| x == "-e"), "and no tool server: {q:?}");
    }

    /// A Deep-mode question reads with the agent's own tools and never has our server.
    #[test]
    fn a_question_reads_with_the_agents_own_tools() {
        let (q, _) = args(&BackendOptions::default(), false, true, false, None);
        assert_eq!(after(&q, "--tools"), READ_TOOLS.join(","));
        assert!(!q.iter().any(|x| x == "-e"), "{q:?}");
    }

    /// The agent is registered under its own id, not handed to the plain adapter.
    #[test]
    fn registered_not_the_plain_adapter() {
        assert!(AgentAdapters::built_in().for_agent("pi").ids().contains(&"pi"));
        assert_eq!(PiCli.required_flags(), (&["--help"][..], &["--mode", "--session-id"][..]));
        assert!(!PiCli.plain_fallback(), "the plain form would boot the terminal UI");
    }

    /// A folder that can't take the extension leaves a path that isn't there, rather than a build
    /// that quietly has no tools.
    #[test]
    fn a_folder_that_cannot_take_the_extension_leaves_a_path_that_is_not_there() {
        let missing = missing_extension();
        assert!(!std::path::Path::new(&missing).exists(), "{missing}");
    }

    /// pi's JSON lines: the session id, the text, both ends of a tool call, and the usage.
    #[test]
    fn text_tool_calls_and_usage_come_from_the_json_lines() {
        let mut p = StreamParser::new(Arc::new(PiCli));
        let header = serde_json::json!({"type":"session","version":3,"id":"01a1bf2c-e839-7317-aef8-423ca8393cdc","cwd":"/w/P"});
        assert!(PiCli.parse(&mut p, &header).is_empty());
        assert_eq!(p.session_id.as_deref(), Some("01a1bf2c-e839-7317-aef8-423ca8393cdc"));
        let text = serde_json::json!({"type":"message_update","usage":{"input":10,"output":0},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Hello"}});
        assert!(matches!(PiCli.parse(&mut p, &text).as_slice(), [AgentEvent::TextDelta { text }] if text == "Hello"));
        let thinking = serde_json::json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"hmm"}});
        assert!(PiCli.parse(&mut p, &thinking).is_empty(), "thinking is not the answer");
        let start = serde_json::json!({"type":"tool_execution_start","toolCallId":"call_1","toolName":"mcp__craft_anything_run__build_file_write","args":{"path":"app/src/main.rs"}});
        assert!(
            matches!(PiCli.parse(&mut p, &start).as_slice(), [AgentEvent::ToolStarted { tool, .. }] if tool == "build_file_write"),
            "our server's prefix is dropped from the name"
        );
        let end = serde_json::json!({"type":"tool_execution_end","toolCallId":"call_1","toolName":"mcp__craft_anything_run__build_file_write","result":{"content":[{"type":"text","text":"Wrote app/src/main.rs"}]},"isError":false});
        assert!(
            matches!(PiCli.parse(&mut p, &end).as_slice(), [AgentEvent::ToolFinished { ok: true, summary, .. }] if summary.contains("Wrote app/src/main.rs")),
            "the result's text is the summary"
        );
        let done = serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":5952,"output":2,"cacheRead":1280,"cacheWrite":0,"reasoning":0,"totalTokens":7234,"cost":{"total":0.25}}}});
        assert!(PiCli.parse(&mut p, &done).is_empty());
        assert_eq!((p.input_tokens, p.output_tokens), (7232, 2), "cached reads count as input");
        assert_eq!(p.cost_usd, Some(0.25));
        assert!(p.saw_result);
        let again = serde_json::json!({"type":"message_end","message":{"role":"user","content":"hi","usage":{"input":99,"output":0}}});
        assert!(PiCli.parse(&mut p, &again).is_empty(), "the person's own message has no usage to count");
        assert_eq!(p.input_tokens, 7232);
    }

    /// A retry that gave up becomes an error in words, with the fix, and a typed failure for the
    /// stop rules.
    #[test]
    fn a_final_retry_failure_becomes_an_error_in_words() {
        let mut p = StreamParser::new(Arc::new(PiCli));
        let line = serde_json::json!({"type":"auto_retry_end","success":false,"attempt":3,"finalError":"401 Unauthorized"});
        let events = PiCli.parse(&mut p, &line);
        assert!(matches!(events.as_slice(), [AgentEvent::Error { message, remediation: Some(_), .. }] if message.contains("401")), "{events:?}");
        assert_eq!(p.failure.map(|f| f.kind), Some(FailureKind::Authentication));
        for (why, kind) in [
            ("429 Too Many Requests", FailureKind::RateLimited),
            ("insufficient credit", FailureKind::Billing),
            ("upstream timed out", FailureKind::Transient),
            ("the model said no", FailureKind::Unknown),
        ] {
            assert_eq!(classify(why), kind, "{why}");
        }
    }

    /// A folder of a test's own, so a run never writes over another's.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("craft-anything-pi-test-{}-{}-{name}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).expect("a test folder");
        dir
    }

    /// The kept copy of a session, as the project would hold it.
    fn kept(id: &str) -> String {
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-11-09T07:55:11.094Z\",\"cwd\":\"/w/P\"}}\n\
             {{\"type\":\"message\",\"id\":\"a\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"build a clock\"}}]}}}}\n\
             {{\"type\":\"message\",\"id\":\"b\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Writing app/index.html\"}},{{\"type\":\"toolCall\",\"id\":\"c\",\"name\":\"build_file_write\",\"arguments\":{{}}}}]}}}}\n"
        )
    }

    /// pi names a working folder between two `--`, with its separators read as `-`.
    #[test]
    fn a_working_folder_is_named_the_way_pi_names_it() {
        assert_eq!(session_folder(std::path::Path::new("/private/tmp/p")), "--private-tmp-p--");
        assert_eq!(session_folder(std::path::Path::new(r"C:\w\p")), "--C--w-p--");
    }

    /// A project carried to another computer gets its builder's conversation back, in the folder
    /// pi looks for it in; a copy of another session is refused.
    #[test]
    fn a_session_travels_with_the_project() {
        let dir = temp_dir("session");
        let id = "01a1bf4d-d376-7776-9210-8cdc5ee15094";
        let cwd = std::path::Path::new("/w/P");
        assert!(session_file_in(&dir, id).is_none(), "nothing here yet");
        assert!(restore_session_in(&dir, id, kept(id).as_bytes(), cwd).expect("put back"), "it was put back");
        let path = session_file_in(&dir, id).expect("found by id");
        assert!(path.ends_with(format!("2026-11-09T07-55-11-094Z_{id}.jsonl")), "{path:?}");
        assert_eq!(path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()), Some("--w-P--"), "{path:?}");
        assert!(!restore_session_in(&dir, id, kept(id).as_bytes(), cwd).expect("already here"), "a second copy is nothing to do");
        // A copy of another session is refused where the session isn't here: nothing is written
        // under this one's name.
        let empty = temp_dir("wrong-session");
        let other = kept("11111111-2222-3333-4444-555555555555");
        assert!(restore_session_in(&empty, id, other.as_bytes(), cwd).is_err(), "another session's file is refused");
        assert!(session_file_in(&empty, id).is_none(), "and nothing was put there");
        let _ = std::fs::remove_dir_all(&empty);
        // An id pi would not use names no session.
        assert!(session_file_in(&dir, "../../etc/passwd").is_none());
        assert!(restore_session_in(&dir, "a/b", kept(id).as_bytes(), cwd).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The copy lands in the folder pi really uses: a linked path (`/tmp` on macOS) is resolved.
    #[test]
    fn a_copy_lands_in_the_real_folder_pi_uses() {
        let dir = temp_dir("canonical");
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("a working folder");
        let id = "01a1bf4d-d376-7776-9210-8cdc5ee15094";
        assert!(restore_session_in(&dir, id, kept(id).as_bytes(), &cwd).expect("put back"));
        let real = std::fs::canonicalize(&cwd).expect("the real path");
        let path = session_file_in(&dir, id).expect("found by id");
        let folder = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str());
        assert_eq!(folder, Some(session_folder(&real).as_str()), "{path:?}");
        // A second run writing under the same id is the one a copy keeps.
        let newer = path.with_file_name(format!("2099-01-01T00-00-00-000Z_{id}.jsonl"));
        std::fs::write(&newer, kept(id)).expect("a newer file");
        assert_eq!(session_file_in(&dir, id).as_deref(), Some(newer.as_path()), "the newest file wins");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a new conversation is told of the earlier one: the newest exchanges, clipped.
    #[test]
    fn a_handoff_keeps_the_newest_exchange_and_leaves_out_tool_calls() {
        let text = pi_handoff(&kept("x"), 16_000).expect("something to tell");
        assert!(text.contains("Craft Anything asked you: build a clock"), "{text}");
        assert!(text.contains("You answered: Writing app/index.html"), "{text}");
        assert!(!text.contains("build_file_write"), "a tool call is not an answer: {text}");
        assert!(pi_handoff("", 16_000).is_none());
        // Only the newest entry fits when the room is small.
        let narrow = pi_handoff(&kept("x"), 40).expect("one entry");
        assert_eq!(narrow, "You answered: Writing app/index.html", "{narrow}");
    }

    /// A server, a tool or a flag with a quote in it must not break the extension's JavaScript.
    #[test]
    fn quotes_are_escaped_for_javascript() {
        let mcp = McpCommand {
            command: r#"C:\it's "here"\craft-anything-cli.exe"#.into(),
            args: vec!["mcp".into(), "--project".into(), r"C:\it's here".into()],
            notes: None,
        };
        let js = extension_js(&mcp);
        assert!(js.contains(r#""C:\\it's \"here\"\\craft-anything-cli.exe""#), "{js}");
        assert!(js.contains(r#""C:\\it's here""#), "{js}");
        assert!(js.contains(&format!("registerMcpServer(\"{SERVER}\"")), "{js}");
        // The bridge's own command survives the quoting too.
        assert!(js.contains(r#""core-action-approve""#), "{js}");
        assert!(js.contains(r#""C:\\it's here""#), "the project argument is quoted: {js}");
    }
}
