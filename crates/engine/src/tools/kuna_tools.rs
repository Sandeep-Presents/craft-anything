//! Thin wrappers over the optional external Kuna decompiler: its function inventory, a
//! one-function decompile, cross-references, strings and a whole-program project export. Every
//! call runs through the process runner (stdin closed, wall and idle deadlines, capped output)
//! with an argv array only — never a shell string. JSON output is parsed and paged by items
//! where Kuna defines it; decompile text is paged by lines. Deep mode only: each call reads
//! inside the original, so none of these ids is listed in `clean_room.rs`. The built-in
//! disassembler never needs Kuna installed.

use super::native_common::{page, params_json, spec};
use super::static_input::{self, builder};
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_evidence::{Authority, Confidence, Coverage, CoverageStatus, ErrorCode, Limitation, Subject, ToolError};
use craft_anything_formats::pe;
use craft_anything_platform::process::nonzero_error;
use craft_anything_platform::{Output, Process};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall-clock deadlines: quick listings 60 s, one function 120 s, a whole export 300 s.
const LIST_SECS: u64 = 60;
const DECOMPILE_SECS: u64 = 120;
const EXPORT_SECS: u64 = 300;
/// `max_functions` bound for an export: the selection travels as repeated `--addr 0xVMA`
/// arguments, so a name containing a comma cannot corrupt it. Each address adds about 16–20
/// characters, so 1000 keeps the whole command line comfortably small; larger exports run
/// unbounded instead, through kuna's own limits.
const MAX_EXPORT_FUNCTIONS: usize = 1000;
/// How many written files an export lists; more are counted, not named.
const MAX_LISTED_FILES: usize = 500;

fn d_items() -> usize {
    200
}

fn d_lines() -> usize {
    2000
}

// ---------------------------------------------------------------------------------------------
// params

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FunctionsParams {
    /// Absolute path of the file; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// Only functions whose name contains this text (case-sensitive).
    #[serde(default)]
    pub contains: Option<String>,
    /// First item of the page.
    #[serde(default)]
    pub offset: usize,
    /// Items per page (1–5000, default 200).
    #[serde(default = "d_items")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecompileParams {
    /// Function name, exactly as `kuna_functions_list` reports it (`sub_1040` counts).
    #[serde(default)]
    pub function: Option<String>,
    /// Function entry address (0x-hex or decimal); exactly one of function and address.
    #[serde(default)]
    pub address: Option<String>,
    /// Ask Kuna for its JSON (code, line mappings, variables) instead of plain text.
    #[serde(default)]
    pub json: bool,
    /// Output language: `auto`, `c` or `rust` (Kuna's own values).
    #[serde(default)]
    pub language: Option<String>,
    /// Analysis preset: `auto`, `reliable`, `aggressive` or `fast` (Kuna's own values).
    #[serde(default)]
    pub mode: Option<String>,
    /// Absolute path of the file; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// First line of the page (the plain-text form; JSON returns the whole function).
    #[serde(default)]
    pub offset: usize,
    /// Lines per page (1–20000, default 2000; the plain-text form only).
    #[serde(default = "d_lines")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct XrefsParams {
    /// What to look from the outside: everything that references this function or address.
    #[serde(default)]
    pub to: Option<String>,
    /// The other direction: what this function or address references.
    #[serde(default)]
    pub from: Option<String>,
    /// Absolute path of the file; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// First item of the page.
    #[serde(default)]
    pub offset: usize,
    /// Items per page (1–5000, default 200).
    #[serde(default = "d_items")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StringsParams {
    /// Shortest string worth listing (at least 1).
    #[serde(default)]
    pub min_length: Option<usize>,
    /// A regular expression Kuna applies to the text it found.
    #[serde(default)]
    pub filter: Option<String>,
    /// Absolute path of the file; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// First item of the page.
    #[serde(default)]
    pub offset: usize,
    /// Items per page (1–5000, default 200).
    #[serde(default = "d_items")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectExportParams {
    /// Absolute path of the file; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// Absolute path of the folder to write the project into; it must not exist yet (Kuna creates it).
    pub output_dir: String,
    /// Select the first functions by address (1–1000), skipping PE import slots.
    /// Omit this to let Kuna select the functions for the whole program.
    #[serde(default)]
    pub max_functions: Option<usize>,
    /// Kuna's per-function wall-clock budget in seconds (`--max-fn-seconds`; 0 disables it);
    /// Kuna's own policy when omitted. This tool's own deadline stays at 300 s regardless.
    #[serde(default)]
    pub max_seconds: Option<u64>,
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            "kuna_functions_list",
            "Kuna function inventory",
            "Run the installed Kuna decompiler's function inventory (`kuna functions <file> --json`; absolute path or the active target) and page it; optional contains keeps only functions whose name has that text. Deep mode only: it reads inside the original. Optional engine: the built-in disassembler needs nothing installed.",
            "List the functions the Kuna decompiler finds in a program.",
            Toolset::Kuna,
            Effects::NATIVE_EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Engine("kuna"),
            schema_of::<FunctionsParams>,
            kuna_functions_list,
        ),
        spec(
            "kuna_decompile",
            "Kuna decompile",
            "Decompile one function with the installed Kuna (`kuna decompile <file> <name>` or `kuna decompile <file> <0x-address> --addr`; absolute path or the active target), exactly one of name or address; optional language (auto, c, rust), mode (auto, reliable, aggressive, fast) and json (Kuna's structured output with line mappings). Plain text is paged by lines. Deep mode only. Optional engine.",
            "Turn one function back into readable code with the Kuna decompiler.",
            Toolset::Kuna,
            Effects::NATIVE_EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Engine("kuna"),
            schema_of::<DecompileParams>,
            kuna_decompile,
        ),
        spec(
            "kuna_xrefs",
            "Kuna cross-references",
            "Ask the installed Kuna for cross-references (`kuna xrefs <file> --to X` or `--from X`, a function name or a 0x-address; absolute path or the active target), exactly one direction, paged. Deep mode only. Optional engine.",
            "Ask the Kuna decompiler what points at a function, or what it points at.",
            Toolset::Kuna,
            Effects::NATIVE_EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Engine("kuna"),
            schema_of::<XrefsParams>,
            kuna_xrefs,
        ),
        spec(
            "kuna_strings",
            "Kuna strings",
            "Run the installed Kuna's string listing (`kuna strings <file> --json`; absolute path or the active target) with optional min_length and filter (a regular expression Kuna applies), paged. Deep mode only. Optional engine.",
            "List the text the Kuna decompiler finds in a program.",
            Toolset::Kuna,
            Effects::NATIVE_EVIDENCE.union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Engine("kuna"),
            schema_of::<StringsParams>,
            kuna_strings,
        ),
        spec(
            "kuna_project_export",
            "Kuna project export",
            "Export a whole program to a project folder with the installed Kuna (`kuna decompile-project <file> -o <dir>`; absolute path or the active target). The output folder must not exist yet: Kuna creates it and writes C, header, assembly and README files; max_functions bounds the export to the first functions by address and max_seconds is Kuna's per-function budget. Writes to the filesystem. Deep mode only. Optional engine.",
            "Export a whole program to a folder of C files with the Kuna decompiler.",
            Toolset::Kuna,
            Effects::MUTATES_SESSION.union(Effects::LAUNCHES_PROCESS).union(Effects::WRITES_FILESYSTEM).union(Effects::CARRIES_TARGET_CONTENT),
            Requires::Engine("kuna"),
            schema_of::<ProjectExportParams>,
            kuna_project_export,
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// shared plumbing

/// Use the same locator as the tool availability check so a `KUNA_COMMAND` pin
/// or a Settings override selects the same executable at launch.
fn kuna_exe(ctx: &Ctx<'_>) -> Result<PathBuf, ToolError> {
    ctx.session.settings.engine_path("kuna").ok_or_else(|| {
        ToolError::unavailable(format!("{} is not installed.", craft_anything_engines::display_name("kuna")))
            .with_remediation("Install kuna and set its path in Settings → Engines.")
    })
}

/// Run Kuna through the process runner: stdin closed, wall and idle deadlines, output capped and
/// drained, the child killed on timeout, cancel or drop.
fn run(ctx: &Ctx<'_>, exe: &Path, args: &[String], secs: u64) -> Result<Output, ToolError> {
    let cancel = ctx.cancel.clone();
    Process::new(exe)
        .args(args)
        .timeout(Duration::from_secs(secs))
        .idle_timeout(Duration::from_secs(secs))
        .stdout_cap(16 << 20)
        .stderr_cap(1 << 20)
        .cancel_when(move || cancel.is_cancelled())
        .label("kuna")
        .run()
}

/// Kuna's `--json` output, or a clear error when it isn't JSON this build can read.
fn json_output(out: &Output, what: &str) -> Result<Value, ToolError> {
    if out.stdout_truncated {
        return Err(ToolError::new(ErrorCode::OverLimit, format!("kuna's {what} output exceeded 16 MiB."))
            .with_remediation("Narrow the request where possible, or use text output for decompilation."));
    }
    serde_json::from_slice(&out.stdout).map_err(|_| garbled(what))
}

/// One array field of Kuna's JSON, or a clear error.
fn json_array(v: &Value, field: &str, what: &str) -> Result<Vec<Value>, ToolError> {
    v.get(field).and_then(Value::as_array).cloned().ok_or_else(|| garbled(what))
}

fn garbled(what: &str) -> ToolError {
    ToolError::new(ErrorCode::GarbledOutput, format!("kuna's {what} isn't the JSON this build expects."))
        .with_remediation("The installed kuna may be a different version; update kuna or Craft Anything.")
}

/// The run facts every Kuna evidence record carries.
fn meta(exe: &Path, args: &[String], out: &Output) -> Value {
    json!({
        "tool": exe,
        "arguments": args,
        "exit_code": out.code,
        "success": out.success,
        "duration_ms": out.duration.as_millis() as u64,
        "stderr": stderr_head(out),
    })
}

/// About 64 KiB of stderr, cut on a character boundary.
fn stderr_head(out: &Output) -> String {
    out.stderr_text().chars().take(64 << 10).collect()
}

/// The limitation every Kuna result carries.
fn base_lims() -> Vec<Limitation> {
    vec![Limitation::new(
        "raw_tool_output",
        "This is the external Kuna decompiler's own output; its format can change between versions and is not checked by Craft Anything.",
    )]
}

/// A non-empty optional string, or a clear error (an empty value is never silently dropped).
fn text_flag(value: Option<&str>, what: &str) -> Result<Option<String>, ToolError> {
    match value.map(str::trim) {
        None => Ok(None),
        Some("") => Err(ToolError::invalid(format!("{what} is empty")).with_remediation(format!("Pass a value for {what}, or leave it out."))),
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// Escape regex metacharacters so a literal substring stays literal in Kuna's `--filter`.
fn escape_regex(s: &str) -> String {
    const META: &[char] = &['\\', '.', '^', '$', '|', '?', '*', '+', '(', ')', '[', ']', '{', '}'];
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if META.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Evidence for one page of a JSON list Kuna returned.
#[allow(clippy::too_many_arguments)]
fn paged_evidence(
    ctx: &Ctx<'_>,
    op: &str,
    subject: Subject,
    params: Value,
    exe: &Path,
    args: &[String],
    out: &Output,
    field: &str,
    items: &[Value],
    offset: usize,
    limit: usize,
    summary: String,
) -> Result<ToolOutput, ToolError> {
    let (page_items, cov) = page(items, offset, limit);
    let mut result = meta(exe, args, out);
    if let Value::Object(m) = &mut result {
        m.insert(field.to_string(), Value::Array(page_items));
        m.insert("offset".into(), json!(offset));
        m.insert("limit".into(), json!(limit));
        m.insert("total".into(), json!(items.len()));
    }
    let b = builder(ctx, op, subject, params)
        .result(result)
        .summary(summary)
        .confidence(Confidence::Observed)
        .authority(Authority::OutsideService)
        .limitations(base_lims())
        .coverage(cov);
    Ok(ToolOutput::evidence(b.build()?))
}

/// Evidence for Kuna's raw text output, paged by lines.
#[allow(clippy::too_many_arguments)]
fn text_evidence(
    ctx: &Ctx<'_>,
    op: &str,
    subject: Subject,
    params: Value,
    exe: &Path,
    args: &[String],
    out: &Output,
    offset: usize,
    limit: usize,
    headline: String,
) -> Result<ToolOutput, ToolError> {
    let text = out.stdout_text();
    let limit = limit.clamp(1, 20_000);
    let total = text.lines().count();
    let page: Vec<&str> = text.lines().skip(offset).take(limit).collect();
    let mut lims = base_lims();
    if out.stdout_truncated {
        lims.push(Limitation::new("output_capped", "kuna printed more than 16 MiB; the rest was cut off. Narrow the request (one function)."));
    }
    if String::from_utf8(out.stdout.clone()).is_err() {
        lims.push(Limitation::new("invalid_utf8", "Some output bytes were not valid UTF-8 and are shown as replacement characters."));
    }
    if !out.success {
        lims.push(Limitation::new("nonzero_exit", format!("The tool exited with {:?}; see stderr.", out.code)));
    }
    let b = builder(ctx, op, subject, params)
        .result(json!({
            "tool": exe,
            "arguments": args,
            "exit_code": out.code,
            "success": out.success,
            "duration_ms": out.duration.as_millis() as u64,
            "total_lines": total,
            "offset": offset,
            "limit": limit,
            "output": page.join("\n"),
            "stderr": stderr_head(out),
        }))
        .summary(format!("{headline}: {total} lines of output{}.", if out.success { String::new() } else { format!(", exit code {:?}", out.code) }))
        .confidence(Confidence::Observed)
        .authority(Authority::OutsideService)
        .limitations(lims)
        .coverage(Coverage::page(page.len() as u64, total as u64, offset as u64));
    Ok(ToolOutput::evidence(b.build()?))
}

// ---------------------------------------------------------------------------------------------
// argv

/// `kuna functions <file> --json [--filter <literal>]`: `contains` becomes a literal regex.
fn functions_args(file: &Path, contains: Option<&str>) -> Result<Vec<String>, ToolError> {
    let mut args = vec!["functions".to_string(), file.display().to_string(), "--json".to_string()];
    if let Some(c) = text_flag(contains, "contains")? {
        args.extend(["--filter".to_string(), escape_regex(&c)]);
    }
    Ok(args)
}

/// `kuna functions <file> --json --sort addr --limit N` — the inventory that bounds an export.
fn functions_page_args(file: &Path, n: usize) -> Vec<String> {
    vec![
        "functions".to_string(),
        file.display().to_string(),
        "--json".to_string(),
        "--sort".to_string(),
        "addr".to_string(),
        "--limit".to_string(),
        n.to_string(),
    ]
}

/// `kuna decompile <file> <name|0x-addr> [--addr] [--json] [--language L] [--mode M]`;
/// exactly one of a function name and an address.
fn decompile_args(file: &Path, p: &DecompileParams) -> Result<(Vec<String>, String), ToolError> {
    let (target, by_address) = match (&p.function, &p.address) {
        (Some(_), Some(_)) => {
            return Err(ToolError::invalid("pass either function or address, not both")
                .with_remediation("kuna decompile takes exactly one of a function name and a 0x address."));
        }
        (None, None) => {
            return Err(ToolError::invalid("kuna decompile needs a function name or an address")
                .with_remediation("Pass function (a name kuna_functions_list reports) or address (0x-hex), exactly one of the two."));
        }
        (Some(n), None) => {
            let n = n.trim();
            if n.is_empty() || n.starts_with('-') || n.chars().any(char::is_control) {
                return Err(ToolError::invalid(format!("{n:?} is not a function name"))
                    .with_remediation("Pass a name kuna_functions_list reports, or use the address parameter."));
            }
            (n.to_string(), false)
        }
        (None, Some(a)) => (format!("{:#x}", super::parse_address(a)?), true),
    };
    let mut args = vec!["decompile".to_string(), file.display().to_string(), target.clone()];
    if by_address {
        args.push("--addr".to_string());
    }
    if p.json {
        args.push("--json".to_string());
    }
    if let Some(l) = text_flag(p.language.as_deref(), "language")? {
        args.extend(["--language".to_string(), l]);
    }
    if let Some(m) = text_flag(p.mode.as_deref(), "mode")? {
        args.extend(["--mode".to_string(), m]);
    }
    Ok((args, target))
}

/// `kuna xrefs <file> (--to X | --from X) --json`; exactly one direction.
fn xrefs_args(file: &Path, p: &XrefsParams) -> Result<(Vec<String>, String), ToolError> {
    let (flag, x) = match (&p.to, &p.from) {
        (Some(_), Some(_)) => {
            return Err(ToolError::invalid("pass either to or from, not both")
                .with_remediation("kuna xrefs answers one direction: what points at a target, or what it points at."));
        }
        (None, None) => {
            return Err(ToolError::invalid("kuna xrefs needs to or from")
                .with_remediation("Pass to (everything that references a function or address) or from (what it references), exactly one."));
        }
        (Some(t), None) => ("--to", t),
        (None, Some(f)) => ("--from", f),
    };
    let x = x.trim();
    if x.is_empty() || x.starts_with('-') || x.chars().any(char::is_control) {
        return Err(
            ToolError::invalid(format!("{x:?} is not a function name or address")).with_remediation("Pass a name kuna_functions_list reports or a 0x address.")
        );
    }
    Ok((
        vec!["xrefs".to_string(), file.display().to_string(), flag.to_string(), x.to_string(), "--json".to_string()],
        format!("{} {x}", flag.trim_start_matches('-')),
    ))
}

/// `kuna strings <file> --json [--min-length N] [--filter REGEX]`.
fn strings_args(file: &Path, p: &StringsParams) -> Result<Vec<String>, ToolError> {
    let mut args = vec!["strings".to_string(), file.display().to_string(), "--json".to_string()];
    if let Some(n) = p.min_length {
        if n == 0 {
            return Err(ToolError::invalid("min_length must be at least 1").with_remediation("Pass the shortest string worth listing, for example 5."));
        }
        args.extend(["--min-length".to_string(), n.to_string()]);
    }
    if let Some(f) = text_flag(p.filter.as_deref(), "filter")? {
        args.extend(["--filter".to_string(), f]);
    }
    Ok(args)
}

/// `kuna decompile-project <file> -o <dir> [--addr 0xVMA].. [--max-fn-seconds N]`.
fn export_args(file: &Path, dir: &Path, addresses: &[String], max_seconds: Option<u64>) -> Vec<String> {
    let mut args = vec!["decompile-project".to_string(), file.display().to_string(), "-o".to_string(), dir.display().to_string()];
    for a in addresses {
        args.extend(["--addr".to_string(), a.clone()]);
    }
    if let Some(s) = max_seconds {
        args.extend(["--max-fn-seconds".to_string(), s.to_string()]);
    }
    args
}

// ---------------------------------------------------------------------------------------------
// tools

/// Page the filtered inventory, so the offsets and total count describe matching functions.
fn kuna_functions_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: FunctionsParams = parse_params(params)?;
    let exe = kuna_exe(ctx)?;
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)?;
    let args = functions_args(&input.path, p.contains.as_deref())?;
    ctx.report(None, format!("Running kuna functions on {}", input.name));
    let out = run(ctx, &exe, &args, LIST_SECS)?;
    ctx.cancel.check()?;
    if !out.success {
        return Err(nonzero_error("kuna", &out));
    }
    let v = json_output(&out, "function inventory")?;
    let items = json_array(&v, "functions", "function inventory")?;
    let total = items.len();
    paged_evidence(
        ctx,
        "kuna_functions_list",
        input.subject,
        params_json(&p),
        &exe,
        &args,
        &out,
        "functions",
        &items,
        p.offset,
        p.limit,
        format!("kuna found {total} functions in {}.", input.name),
    )
}

/// Keep usable decompile output on a failed run and retain the exit status in its evidence.
fn kuna_decompile(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: DecompileParams = parse_params(params)?;
    let exe = kuna_exe(ctx)?;
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)?;
    let (args, target) = decompile_args(&input.path, &p)?;
    ctx.report(None, format!("Decompiling {target} with kuna"));
    let out = run(ctx, &exe, &args, DECOMPILE_SECS)?;
    ctx.cancel.check()?;
    if !out.success && out.stdout.is_empty() {
        return Err(nonzero_error("kuna", &out));
    }
    if p.json {
        let v = match json_output(&out, "decompile") {
            Ok(v) => v,
            // A failed run that didn't print JSON is a plain run failure, not a version gap.
            Err(_) if !out.success => return Err(nonzero_error("kuna", &out)),
            Err(e) => return Err(e),
        };
        let fns = json_array(&v, "functions", "decompile")?;
        let n = fns.len();
        let mut result = meta(&exe, &args, &out);
        if let Value::Object(m) = &mut result {
            m.insert("functions".to_string(), Value::Array(fns));
            m.insert("count".to_string(), json!(n));
        }
        let mut lims = base_lims();
        if !out.success {
            lims.push(Limitation::new(
                "nonzero_exit",
                format!("The tool exited with {:?}; see stderr. The functions above are what it decompiled before that.", out.code),
            ));
        }
        let b = builder(ctx, "kuna_decompile", input.subject, params_json(&p))
            .result(result)
            .summary(format!("kuna decompile of {target}: {n} function{}.", if n == 1 { "" } else { "s" }))
            .confidence(Confidence::Observed)
            .authority(Authority::OutsideService)
            .limitations(lims)
            .coverage(if out.success {
                Coverage::complete(n as u64)
            } else {
                Coverage { status: CoverageStatus::Partial, examined: Some(n as u64), total: None, next_offset: None }
            });
        return Ok(ToolOutput::evidence(b.build()?));
    }
    text_evidence(ctx, "kuna_decompile", input.subject, params_json(&p), &exe, &args, &out, p.offset, p.limit, format!("kuna decompile of {target}"))
}

/// Page references in the requested direction. The opposite direction is a separate lookup.
fn kuna_xrefs(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: XrefsParams = parse_params(params)?;
    let exe = kuna_exe(ctx)?;
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)?;
    let (args, direction) = xrefs_args(&input.path, &p)?;
    ctx.report(None, format!("Running kuna xrefs {direction}"));
    let out = run(ctx, &exe, &args, LIST_SECS)?;
    ctx.cancel.check()?;
    if !out.success {
        return Err(nonzero_error("kuna", &out));
    }
    let v = json_output(&out, "cross-references")?;
    let items = json_array(&v, "xrefs", "cross-references")?;
    let total = items.len();
    paged_evidence(
        ctx,
        "kuna_xrefs",
        input.subject,
        params_json(&p),
        &exe,
        &args,
        &out,
        "xrefs",
        &items,
        p.offset,
        p.limit,
        format!("kuna found {total} references {direction} in {}.", input.name),
    )
}

/// Apply the regex and minimum length in Kuna before paging the matching strings.
fn kuna_strings(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: StringsParams = parse_params(params)?;
    let exe = kuna_exe(ctx)?;
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)?;
    let args = strings_args(&input.path, &p)?;
    ctx.report(None, format!("Running kuna strings on {}", input.name));
    let out = run(ctx, &exe, &args, LIST_SECS)?;
    ctx.cancel.check()?;
    if !out.success {
        return Err(nonzero_error("kuna", &out));
    }
    let v = json_output(&out, "string listing")?;
    let items = json_array(&v, "strings", "string listing")?;
    let total = items.len();
    paged_evidence(
        ctx,
        "kuna_strings",
        input.subject,
        params_json(&p),
        &exe,
        &args,
        &out,
        "strings",
        &items,
        p.offset,
        p.limit,
        format!("kuna found {total} strings in {}.", input.name),
    )
}

/// Run Kuna's export and record its artifacts. Individual decompiles may fail even on exit zero.
fn kuna_project_export(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ProjectExportParams = parse_params(params)?;
    let exe = kuna_exe(ctx)?;
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)?;
    let dir = PathBuf::from(&p.output_dir);
    if !dir.is_absolute() {
        return Err(ToolError::invalid(format!("{:?} is not an absolute path", p.output_dir))
            .with_remediation("Pass the full path of the folder kuna should create, for example /home/me/prog.kuna."));
    }
    if dir.exists() {
        return Err(ToolError::invalid(format!("{} already exists; kuna would overwrite it.", dir.display()))
            .with_remediation("Choose a folder that doesn't exist yet; kuna creates it and writes the project files."));
    }
    let (addresses, bounded) = match p.max_functions {
        None => (Vec::new(), false),
        Some(n) => {
            let n = n.clamp(1, MAX_EXPORT_FUNCTIONS);
            ctx.report(Some(0.1), format!("Bounding the export to {n} functions"));
            let import_slots = pe_import_slots(&input.bytes);
            // Leave room for import slots and one extra candidate to detect a shortened export.
            let fargs = functions_page_args(&input.path, n.saturating_add(import_slots.len()).saturating_add(1));
            let out = run(ctx, &exe, &fargs, LIST_SECS)?;
            ctx.cancel.check()?;
            if !out.success {
                return Err(nonzero_error("kuna", &out));
            }
            let v = json_output(&out, "function inventory")?;
            let inventory = json_array(&v, "functions", "function inventory")?;
            let mut candidates = inventory
                .iter()
                .filter_map(|f| f.get("address_hex").and_then(Value::as_str))
                .filter(|hex| super::parse_address(hex).is_ok_and(|address| !import_slots.contains(&address)));
            let addresses: Vec<String> = candidates.by_ref().take(n).map(str::to_owned).collect();
            if addresses.is_empty() {
                return Err(ToolError::invalid(format!("kuna found no functions in {} to export.", input.name))
                    .with_remediation("Check the file is a program kuna reads (ELF, PE, Mach-O); run triage to see what it is."));
            }
            (addresses, candidates.next().is_some())
        }
    };
    let args = export_args(&input.path, &dir, &addresses, p.max_seconds);
    ctx.report(Some(0.2), format!("Exporting {} with kuna to {}", input.name, dir.display()));
    let out = run(ctx, &exe, &args, EXPORT_SECS)?;
    ctx.cancel.check()?;
    if !out.success {
        return Err(nonzero_error("kuna", &out));
    }
    let (files, total) = listed_files(&dir)?;
    let mut lims = base_lims();
    lims.push(Limitation::new(
        "filesystem_write",
        format!("kuna wrote the project folder {}; the files are kuna's own output and not checked by Craft Anything.", dir.display()),
    ));
    lims.push(Limitation::new("function_coverage_unverified", "Kuna can skip individual functions even when the export succeeds. See the generated report."));
    if bounded {
        lims.push(Limitation::new(
            "functions_bounded",
            format!("The export selected the first {} functions by address. Omit max_functions to export the whole program.", addresses.len()),
        ));
    }
    if total > files.len() {
        lims.push(Limitation::new("files_list_truncated", format!("{total} files were written; the first {} are listed.", files.len())));
    }
    let mut result = meta(&exe, &args, &out);
    let listed = files.len();
    if let Value::Object(m) = &mut result {
        m.insert("output_dir".to_string(), json!(dir.display().to_string()));
        m.insert("files".to_string(), Value::Array(files));
        m.insert("total_files".to_string(), json!(total));
        m.insert("stdout".to_string(), json!(out.stdout_text().chars().take(4096).collect::<String>()));
    }
    let coverage = match (bounded, total > listed) {
        (true, _) => Coverage { status: CoverageStatus::Partial, ..Coverage::default() },
        (false, true) => Coverage { status: CoverageStatus::Partial, examined: Some(listed as u64), total: Some(total as u64), next_offset: None },
        (false, false) => Coverage::default(),
    };
    let b = builder(ctx, "kuna_project_export", input.subject, params_json(&p))
        .result(result)
        .summary(format!("kuna exported {total} files to {}.", dir.display()))
        .confidence(Confidence::Observed)
        .authority(Authority::OutsideService)
        .limitations(lims)
        .coverage(coverage);
    Ok(ToolOutput::evidence(b.build()?))
}

/// The inventory includes import pointers even when a slot sits in a code section.
fn pe_import_slots(bytes: &[u8]) -> HashSet<u64> {
    match pe::headers(bytes) {
        Ok(headers) => pe::import_table(bytes, &headers).rows.into_iter().filter_map(|row| row.slot_va).collect(),
        Err(_) => HashSet::new(),
    }
}

/// The files an export wrote: name and size, sorted, at most [`MAX_LISTED_FILES`] of them.
fn listed_files(dir: &Path) -> Result<(Vec<Value>, usize), ToolError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::new(ErrorCode::GarbledOutput, format!("kuna finished but {} wasn't created.", dir.display()))
                .with_remediation("The installed kuna may be a different version; update kuna or Craft Anything."));
        }
        Err(e) => return Err(ToolError::io(dir.display(), e)),
    };
    let mut files: Vec<(String, u64)> = entries
        .flatten()
        .map(|entry| {
            let size = entry.metadata().map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0);
            (entry.file_name().to_string_lossy().into_owned(), size)
        })
        .collect();
    files.sort();
    let total = files.len();
    let listed: Vec<Value> = files.into_iter().take(MAX_LISTED_FILES).map(|(name, bytes)| json!({"name": name, "bytes": bytes})).collect();
    Ok((listed, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::{call, call_err};
    use crate::{CallOptions, Caller, Engine};
    use serde_json::json;

    #[test]
    fn argv_shapes_match_kunas_cli() {
        let f = Path::new("/x/prog");
        assert_eq!(functions_args(f, None).unwrap(), vec!["functions", "/x/prog", "--json"]);
        assert_eq!(
            functions_args(f, Some("a.b*")).unwrap(),
            vec!["functions", "/x/prog", "--json", "--filter", "a\\.b\\*"],
            "contains is escaped into a literal regex"
        );
        assert!(functions_args(f, Some("  ")).is_err(), "blank contains is refused");

        let d = |function: Option<&str>, address: Option<&str>, json: bool| DecompileParams {
            function: function.map(str::to_string),
            address: address.map(str::to_string),
            json,
            language: None,
            mode: None,
            path: None,
            offset: 0,
            limit: 5,
        };
        assert_eq!(decompile_args(f, &d(Some(" main "), None, false)).unwrap().0, vec!["decompile", "/x/prog", "main"]);
        assert_eq!(decompile_args(f, &d(Some("main"), None, true)).unwrap().0, vec!["decompile", "/x/prog", "main", "--json"]);
        assert_eq!(decompile_args(f, &d(None, Some("0x1040"), false)).unwrap().0, vec!["decompile", "/x/prog", "0x1040", "--addr"]);
        assert_eq!(decompile_args(f, &d(None, Some("4160"), false)).unwrap().0, vec!["decompile", "/x/prog", "0x1040", "--addr"]);
        assert!(decompile_args(f, &d(Some("a"), Some("0x1"), false)).is_err(), "both name and address");
        assert!(decompile_args(f, &d(None, None, false)).is_err(), "neither name nor address");
        assert!(decompile_args(f, &d(Some("-x"), None, false)).is_err(), "flag-looking name");
        assert!(decompile_args(f, &d(Some("a\nb"), None, false)).is_err(), "control character");
        let mut full = d(Some("main"), None, true);
        full.language = Some("rust".into());
        full.mode = Some("fast".into());
        assert_eq!(decompile_args(f, &full).unwrap().0, vec!["decompile", "/x/prog", "main", "--json", "--language", "rust", "--mode", "fast"]);
        let mut blank = d(Some("main"), None, false);
        blank.language = Some("  ".into());
        assert!(decompile_args(f, &blank).is_err(), "blank language is refused, not dropped");

        let x =
            |to: Option<&str>, from: Option<&str>| XrefsParams { to: to.map(str::to_string), from: from.map(str::to_string), path: None, offset: 0, limit: 50 };
        assert_eq!(xrefs_args(f, &x(Some("main"), None)).unwrap().0, vec!["xrefs", "/x/prog", "--to", "main", "--json"]);
        assert_eq!(xrefs_args(f, &x(None, Some("0x1040"))).unwrap().0, vec!["xrefs", "/x/prog", "--from", "0x1040", "--json"]);
        assert!(xrefs_args(f, &x(Some("a"), Some("b"))).is_err(), "both directions");
        assert!(xrefs_args(f, &x(None, None)).is_err(), "no direction");
        assert!(xrefs_args(f, &x(Some(""), None)).is_err(), "blank direction");

        let mut s = StringsParams { min_length: None, filter: None, path: None, offset: 0, limit: 50 };
        assert_eq!(strings_args(f, &s).unwrap(), vec!["strings", "/x/prog", "--json"]);
        s.min_length = Some(5);
        s.filter = Some("pass.".into());
        assert_eq!(strings_args(f, &s).unwrap(), vec!["strings", "/x/prog", "--json", "--min-length", "5", "--filter", "pass."]);
        s.min_length = Some(0);
        assert!(strings_args(f, &s).is_err(), "min_length 0 is refused");
        s.min_length = None;
        s.filter = Some(String::new());
        assert!(strings_args(f, &s).is_err(), "blank filter is refused");

        let dir = Path::new("/x/out");
        assert_eq!(export_args(f, dir, &[], None), vec!["decompile-project", "/x/prog", "-o", "/x/out"]);
        assert_eq!(
            export_args(f, dir, &["0x1000".to_string(), "0x1020".to_string()], Some(30)),
            vec!["decompile-project", "/x/prog", "-o", "/x/out", "--addr", "0x1000", "--addr", "0x1020", "--max-fn-seconds", "30"]
        );
        assert_eq!(functions_page_args(f, 2), vec!["functions", "/x/prog", "--json", "--sort", "addr", "--limit", "2"]);
    }

    /// A stand-in kuna that records its argv and answers with canned JSON.
    #[cfg(unix)]
    fn fake_kuna(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("kuna");
        let body = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" >> {log}\n\
             case \"$1\" in\n\
               functions) cat {functions} ;;\n\
               decompile) case \" $* \" in *' --json '*) cat {decompile} ;; *) printf 'int main(void)\\n{{\\n  return 0x2a;\\n}}\\n' ;; esac ;;\n\
               xrefs) cat {xrefs} ;;\n\
               strings) cat {strings} ;;\n\
               decompile-project) mkdir -p \"$4\" && printf 'int main(void){{}}\\n' > \"$4/prog.c\" && printf 'wrote %s\\n' \"$4\" ;;\n\
               *) echo \"fake kuna: $1\" >&2 ; exit 2 ;;\n\
             esac\n",
            log = dir.join("argv.log").display(),
            functions = dir.join("functions.json").display(),
            decompile = dir.join("decompile.json").display(),
            xrefs = dir.join("xrefs.json").display(),
            strings = dir.join("strings.json").display(),
        );
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// The canned inventories the fake answers with, shaped like kuna 1.985's own JSON.
    #[cfg(unix)]
    fn canned_json(dir: &Path) {
        std::fs::write(
            dir.join("functions.json"),
            r#"{"binary":"prog.bin","count":3,"total":3,"error":null,"functions":[
                {"name":"_init","address":4096,"address_hex":"0x1000","aliases":["sub_1000"],"object_location":null,"size":27},
                {"name":"sub_1020","address":4128,"address_hex":"0x1020","aliases":[],"object_location":null,"size":16},
                {"name":"main","address":4160,"address_hex":"0x1040","aliases":["sub_1040"],"object_location":null,"size":16}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("strings.json"),
            r#"{"binary":"prog.bin","encoding":"ascii","min_length":5,"filter":null,"section":null,
                "termination":"any","scanned":"sections","xrefs":true,"count":2,"strings":[
                {"address":792,"address_hex":"0x318","text":"/lib/ld-linux.so.2","length":18,"byte_length":19,
                 "nul_terminated":true,"encoding":"ascii","section":".interp","xrefs_count":0,"functions":[]},
                {"address":1113,"address_hex":"0x459","text":"__libc_start_main","length":17,"byte_length":18,
                 "nul_terminated":true,"encoding":"ascii","section":".dynstr","xrefs_count":0,"functions":[]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("xrefs.json"),
            r#"{"binary":"prog.bin","target":{"name":"main","address":4160,"address_hex":"0x1040","aliases":[]},
                "direction":"to","count":1,"xrefs":[
                {"address":4200,"address_hex":"0x1068","kind":"data","from_address":4200,"from_address_hex":"0x1068",
                 "to_address":4160,"to_address_hex":"0x1040",
                 "from_function":{"name":"_start","address":4176,"address_hex":"0x1050"},
                 "to_function":{"name":"main","address":4160,"address_hex":"0x1040"},
                 "instruction":"LEA RDI,[0x1040]"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("decompile.json"),
            r#"{"binary":"prog.bin","language":"c-language","count":1,"error":null,"functions":[
                {"name":"main","address":4160,"address_hex":"0x1040","aliases":["sub_1040"],"object_location":null,
                 "size":16,"code":"int main(void)\n{\n  return 0x2a;\n}","error":null,"unstructured_gotos":0,
                 "line_mappings":[{"line_number":3,"addresses":[4165]}],"variables":[],"types":[]}],"assertions":[]}"#,
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn engine_with_fake_kuna(dir: &Path) -> Engine {
        let mut s = crate::testing::session();
        s.settings.engines.tools.kuna = Some(fake_kuna(dir));
        Engine::new(s)
    }

    #[cfg(unix)]
    fn sample() -> PathBuf {
        craft_anything_testkit::temp_file("prog.bin", &craft_anything_testkit::elf64(craft_anything_testkit::ElfArch::X86_64, true).0)
    }

    #[cfg(unix)]
    fn argv_log(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("argv.log")).unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn functions_list_parses_json_and_pages() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let prog = sample();
        let v = call(&e, "kuna_functions_list", json!({"path": prog.to_string_lossy(), "limit": 2}));
        assert_eq!(v["result"]["total"], 3);
        assert_eq!(v["result"]["functions"].as_array().unwrap().len(), 2, "the page is one item shorter than the list");
        assert_eq!(v["result"]["functions"][0]["name"], "_init");
        assert_eq!(v["coverage"]["status"], "partial");
        assert_eq!(v["coverage"]["next_offset"], 2);
        assert_eq!(v["operation"], "kuna_functions_list");
        assert_eq!(argv_log(dir.path()), format!("functions\n{}\n--json\n", prog.display()), "the exact argv reached kuna");
        // A contains filter reaches kuna as an escaped literal regex.
        let v = call(&e, "kuna_functions_list", json!({"path": prog.to_string_lossy(), "contains": "a.b"}));
        assert_eq!(v["result"]["total"], 3, "the fake doesn't filter; the argv is what's asserted");
        assert!(argv_log(dir.path()).ends_with(format!("functions\n{}\n--json\n--filter\na\\.b\n", prog.display()).as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn decompile_pages_text_and_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let prog = sample();
        let v = call(&e, "kuna_decompile", json!({"path": prog.to_string_lossy(), "function": "main"}));
        assert_eq!(v["result"]["total_lines"], 4);
        assert_eq!(v["result"]["output"], "int main(void)\n{\n  return 0x2a;\n}");
        assert_eq!(v["result"]["offset"], 0);
        assert_eq!(argv_log(dir.path()), format!("decompile\n{}\nmain\n", prog.display()));
        let v = call(&e, "kuna_decompile", json!({"path": prog.to_string_lossy(), "address": "0x1040", "json": true}));
        assert_eq!(v["result"]["count"], 1);
        assert_eq!(v["result"]["functions"][0]["name"], "main");
        assert_eq!(v["result"]["functions"][0]["code"], "int main(void)\n{\n  return 0x2a;\n}");
        assert!(argv_log(dir.path()).ends_with(format!("decompile\n{}\n0x1040\n--addr\n--json\n", prog.display()).as_str()));
        // Exactly one of name and address, checked before anything runs.
        let log_before_refused = argv_log(dir.path());
        for bad in [json!({"path": prog.to_string_lossy()}), json!({"path": prog.to_string_lossy(), "function": "main", "address": "0x1040"})] {
            let err = call_err(&e, "kuna_decompile", bad);
            assert!(err.message.contains("function") && err.remediation.is_some(), "{err:?}");
        }
        assert_eq!(argv_log(dir.path()), log_before_refused, "refused params never reach kuna");
    }

    #[cfg(unix)]
    #[test]
    fn xrefs_and_strings_parse_and_page() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let prog = sample();
        let v = call(&e, "kuna_xrefs", json!({"path": prog.to_string_lossy(), "to": "main", "limit": 1}));
        assert_eq!(v["result"]["total"], 1);
        assert_eq!(v["result"]["xrefs"][0]["kind"], "data");
        assert_eq!(v["result"]["xrefs"][0]["from_function"]["name"], "_start");
        assert_eq!(argv_log(dir.path()), format!("xrefs\n{}\n--to\nmain\n--json\n", prog.display()));
        let v = call(&e, "kuna_strings", json!({"path": prog.to_string_lossy(), "min_length": 5, "filter": "libc"}));
        assert_eq!(v["result"]["total"], 2);
        assert_eq!(v["result"]["strings"][1]["text"], "__libc_start_main");
        assert!(argv_log(dir.path()).ends_with(format!("strings\n{}\n--json\n--min-length\n5\n--filter\nlibc\n", prog.display()).as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn project_export_refuses_an_existing_directory_before_running() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let prog = sample();
        let out = dir.path().join("already-there");
        std::fs::create_dir_all(&out).unwrap();
        let err = call_err(&e, "kuna_project_export", json!({"path": prog.to_string_lossy(), "output_dir": out.to_string_lossy()}));
        assert!(err.message.contains("already exists"), "{err:?}");
        assert!(err.remediation.is_some(), "{err:?}");
        assert_eq!(argv_log(dir.path()), "", "kuna never ran");
        let err = call_err(&e, "kuna_project_export", json!({"path": prog.to_string_lossy(), "output_dir": "relative/dir"}));
        assert!(err.message.contains("absolute"), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn project_export_maps_limits_and_lists_written_files() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let prog = sample();
        let out = dir.path().join("prog.kuna");
        let v = call(
            &e,
            "kuna_project_export",
            json!({"path": prog.to_string_lossy(), "output_dir": out.to_string_lossy(), "max_functions": 2, "max_seconds": 30}),
        );
        assert_eq!(v["result"]["total_files"], 1);
        assert_eq!(v["result"]["files"][0]["name"], "prog.c");
        assert_eq!(v["result"]["output_dir"], out.display().to_string());
        assert_eq!(v["coverage"]["status"], "partial");
        assert_eq!(
            argv_log(dir.path()),
            format!(
                "functions\n{p}\n--json\n--sort\naddr\n--limit\n3\ndecompile-project\n{p}\n-o\n{out}\n--addr\n0x1000\n--addr\n0x1020\n--max-fn-seconds\n30\n",
                p = prog.display(),
                out = out.display()
            ),
            "the export is bounded through kuna's own inventory and selection flags"
        );
        assert!(out.join("prog.c").is_file(), "the fake created the folder the way kuna does");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_project_export_skips_pe_import_slots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("functions.json"),
            r#"{"total":3,"functions":[{"address_hex":"0x140006180"},{"address_hex":"0x140006200"},{"address_hex":"0x140007000"}]}"#,
        )
        .unwrap();
        let e = engine_with_fake_kuna(dir.path());
        let prog = craft_anything_testkit::temp_file("prog.exe", &craft_anything_testkit::program_facts::pe_facts());
        let out = dir.path().join("prog.kuna");
        let v = call(&e, "kuna_project_export", json!({"path": prog.to_string_lossy(), "output_dir": out.to_string_lossy(), "max_functions": 1}));
        assert_eq!(
            argv_log(dir.path()),
            format!(
                "functions\n{p}\n--json\n--sort\naddr\n--limit\n7\ndecompile-project\n{p}\n-o\n{out}\n--addr\n0x140007000\n",
                p = prog.display(),
                out = out.display()
            )
        );
        assert_eq!(v["coverage"]["status"], "unknown", "the export did not verify function completion");
    }

    #[test]
    fn missing_kuna_or_missing_input_is_a_clear_error() {
        // No kuna configured: the engine table names it, so the refusal says what to install.
        let e = Engine::new(crate::testing::session());
        let prog = sample_path();
        let err = call_err(&e, "kuna_functions_list", json!({"path": prog}));
        assert_eq!(err.code, craft_anything_evidence::ErrorCode::EngineMissing);
        assert!(err.message.contains("Kuna") && err.message.contains("installed"), "{err:?}");
        assert!(err.remediation.is_some(), "{err:?}");
    }

    fn sample_path() -> String {
        craft_anything_testkit::temp_file("prog.bin", &craft_anything_testkit::elf64(craft_anything_testkit::ElfArch::X86_64, true).0)
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_input_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        canned_json(dir.path());
        let e = engine_with_fake_kuna(dir.path());
        let err = call_err(&e, "kuna_functions_list", json!({"path": dir.path().join("no-such.bin").to_string_lossy()}));
        assert!(err.message.contains("no-such.bin"), "the error names the file: {err:?}");
        assert_eq!(err.code, craft_anything_evidence::ErrorCode::NotFound);
        assert_eq!(argv_log(dir.path()), "", "kuna never ran for a file that isn't there");
    }

    #[test]
    fn clean_room_refuses_every_kuna_tool() {
        let mut s = crate::testing::session();
        s.settings.deep_mode = false;
        let e = Engine::new(s);
        let prog = sample_path();
        for id in ["kuna_functions_list", "kuna_decompile", "kuna_xrefs", "kuna_strings", "kuna_project_export"] {
            let params = if id == "kuna_project_export" { json!({"path": prog, "output_dir": "/tmp/nope-kuna"}) } else { json!({"path": prog}) };
            let err = e.call(id, params, CallOptions::new(Caller::Mcp)).unwrap_err();
            assert!(crate::clean_room::is_refusal(&err), "{id} must be Deep-only: {err:?}");
        }
    }
}
