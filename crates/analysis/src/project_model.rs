//! Bounded adapter from project storage metadata to explicit experimental simulation views.
//! It preserves missing inputs, calendar failures and unsupported resource costs as unresolved.
use crate::project_simulation::{self as sim, Assignment, DefaultProfile, Interval, PhaseState, Provenance, WallClock};
use craft_anything_formats::project_calendar::{self, Calendars, WorkAllocation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const LIMIT: usize = 10_000;
const SEGMENT_LIMIT: usize = 500_000;
const UNSET: i64 = 32_535_244_799;
const SCALE: i128 = 1_000_000;
const MAX_LABEL_BYTES: usize = 4096;
const MAX_DERIVED_TEXT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateMode {
    Planned,
    Best,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePolicy {
    Strict,
    IntervalPreview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileView {
    pub guid: String,
    pub name: String,
    pub base_kind_candidate: Option<u32>,
    pub direction_candidate: Option<u32>,
    pub opacity_percent_candidate: Option<u32>,
    pub strict_profile: DefaultProfile,
    pub preview_profile: DefaultProfile,
    pub unresolved_effects: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    Work,
    Summary,
    Milestone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskView {
    pub guid: String,
    pub parent_guid: Option<String>,
    pub name: String,
    pub display_id: String,
    pub status_storage_value: u32,
    pub schedule_kind: ScheduleKind,
    pub calendar_guid_storage: String,
    pub effective_calendar_guid: Option<String>,
    pub date_storage_windows: [i64; 4],
    pub planned_start: Option<i64>,
    pub planned_finish: Option<i64>,
    pub best_start: Option<i64>,
    pub best_finish: Option<i64>,
    pub duration_storage_scalar: u64,
    pub assignment_count: usize,
    pub declared_policy_notes: Vec<String>,
    pub unresolved_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostItemView {
    pub name: String,
    pub amount_storage: f64,
    pub amount_minor: Option<i64>,
    pub type_storage: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateView {
    pub effective_wallclock: Option<i64>,
    pub items: Vec<CostItemView>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceCostView {
    pub guid: String,
    pub name: String,
    pub kind_storage_byte: Option<u8>,
    /// Raw source unit code only; no scale or engineering-unit conversion is inferred.
    pub unit_storage_byte: Option<u8>,
    pub rates: Vec<RateView>,
    pub unresolved_reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostLine {
    pub task_guid: String,
    pub task_name: String,
    pub resource_guid: Option<String>,
    pub resource_name: String,
    pub planned_units: Option<f64>,
    pub actual_units: Option<f64>,
    pub remaining_units: Option<f64>,
    pub planned_status_units_derived: bool,
    pub budget_minor: Option<i64>,
    pub actual_minor: Option<i64>,
    pub remaining_minor: Option<i64>,
    pub planned_cumulative_minor: Option<i64>,
    pub unresolved_reasons: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    pub lines: Vec<CostLine>,
    pub budget_minor: i64,
    /// Stored task direct Fixed budgets, each task/item counted once. Keep this source subtotal
    /// separate from the experimental resource-assignment forecast below.
    pub direct_budget_minor: i64,
    pub resource_budget_minor: i64,
    pub unresolved_direct_budget_lines: usize,
    pub unresolved_resource_budget_lines: usize,
    pub actual_minor: i64,
    pub remaining_minor: i64,
    pub planned_cumulative_minor: i64,
    pub unresolved_budget_lines: usize,
    pub unresolved_actual_lines: usize,
    pub unresolved_remaining_lines: usize,
    pub unresolved_cumulative_lines: usize,
    pub complete: bool,
    pub semantics: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcePhase {
    pub resource_guid: String,
    pub state: PhaseState,
    pub provenance: Provenance,
    pub unresolved_reason: Option<String>,
    pub unresolved_profile_effects: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimelineSummary {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub resolved_tasks: usize,
    pub unresolved_tasks: usize,
    pub milestone_tasks: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub schema_version: u32,
    pub focus: i64,
    pub date_mode: DateMode,
    pub appearance_policy: AppearancePolicy,
    pub resource_phases: Vec<ResourcePhase>,
    pub costs: CostSummary,
    pub timeline: TimelineSummary,
    pub unresolved_reasons: Vec<String>,
    pub native_parity_verified: bool,
}
impl Frame {
    pub fn page_costs(&self, offset: usize, limit: usize) -> Page<CostLine> {
        page(&self.costs.lines, offset, limit)
    }
    pub fn page_phases(&self, offset: usize, limit: usize) -> Page<ResourcePhase> {
        page(&self.resource_phases, offset, limit)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub offset: usize,
    pub total: usize,
    pub more: bool,
    pub items: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredAssignment {
    resource_guid: String,
    profile_guid: String,
    planned_units: f64,
    #[serde(default)]
    actual_units_storage: Option<f64>,
    #[serde(default)]
    remaining_units_storage: Option<f64>,
}
#[derive(Debug, Clone, Deserialize)]
struct StoredDirect {
    guid: String,
    name: String,
    amount_storage: f64,
    type_storage_value: u32,
}
#[derive(Debug, Clone, Deserialize)]
struct StoredTask {
    guid: String,
    #[serde(default)]
    parent_guid: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_id: String,
    planned_start_wallclock_scalar: u64,
    duration_storage_scalar: u64,
    status_storage_value: u32,
    #[serde(default)]
    calendar_guid_storage: String,
    #[serde(default = "unset_dates")]
    date_storage_windows: [i64; 4],
    #[serde(default)]
    assignments: Vec<StoredAssignment>,
    #[serde(default)]
    direct_costs: Vec<StoredDirect>,
}
fn unset_dates() -> [i64; 4] {
    [UNSET; 4]
}
#[derive(Debug, Clone)]
struct PreparedTask {
    stored: StoredTask,
    planned: Option<WorkAllocation>,
}
#[derive(Debug, Clone)]
struct PreparedLine {
    line: CostLine,
    task_index: usize,
    resource_index: Option<usize>,
    planned_units: Option<u64>,
    fixed_direct: bool,
}

#[derive(Debug)]
pub struct ProjectModel {
    pub tasks: Vec<TaskView>,
    pub resources: Vec<ResourceCostView>,
    pub model_errors: Vec<String>,
    pub profiles: Vec<ProfileView>,
    prepared: Vec<PreparedTask>,
    assignments: Vec<Assignment>,
    preview_assignments: Vec<Assignment>,
    resource_profile_effects: BTreeMap<String, Vec<String>>,
    lines: Vec<PreparedLine>,
}

fn valid_date(value: i64) -> Option<i64> {
    (value != UNSET).then_some(value)
}
fn nil(guid: &str) -> bool {
    guid.is_empty() || guid.bytes().all(|b| b == b'0' || b == b'-')
}
fn identifier(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 { Err("Invalid or oversized project identifier".into()) } else { Ok(()) }
}
fn check_label(value: &str) -> Result<(), String> {
    if value.len() > MAX_LABEL_BYTES { Err("A project label exceeds 4096 bytes; shorten labels before simulation".into()) } else { Ok(()) }
}
fn charge_text(used: &mut usize, bytes: usize) -> Result<(), String> {
    *used = used.checked_add(bytes).ok_or("Derived project text size overflows")?;
    if *used > MAX_DERIVED_TEXT_BYTES {
        Err("Derived simulation text exceeds 4 MiB; reduce task labels or assignment count before simulation".into())
    } else {
        Ok(())
    }
}
fn charge_line(used: &mut usize, line: &CostLine) -> Result<(), String> {
    charge_text(used, line.task_guid.len())?;
    charge_text(used, line.task_name.len())?;
    charge_text(used, line.resource_guid.as_ref().map_or(0, String::len))?;
    charge_text(used, line.resource_name.len())?;
    for reason in &line.unresolved_reasons {
        charge_text(used, reason.len())?;
    }
    // Reserve bounded space for a focus-dependent cumulative-allocation diagnostic in Frame.
    charge_text(used, 256)
}
fn millionths(value: f64) -> Result<u64, String> {
    if !value.is_finite() || !(0. ..=1_000_000_000.).contains(&value) {
        return Err("Unit quantity is missing, negative, nonfinite or exceeds the supported bound".into());
    }
    Ok((value * 1_000_000.).round() as u64)
}
fn minor(value: f64) -> Option<i64> {
    (value.is_finite() && (0. ..=10_000_000_000.).contains(&value)).then(|| (value * 100.).round() as i64)
}
fn rounded(n: i128, d: i128) -> Result<i64, String> {
    if d <= 0 || n < 0 {
        return Err("Invalid cost rational".into());
    }
    let v = n.checked_add(d / 2).ok_or("Cost rounding overflows")? / d;
    i64::try_from(v).map_err(|_| "Cost exceeds the supported integer amount".into())
}
fn decode_hex(value: &str, limit: usize) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) || value.len() / 2 > limit {
        return Err("Raw storage hex exceeds its size bound or has odd length".into());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |b: u8| match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                b'A'..=b'F' => Some(b - b'A' + 10),
                _ => None,
            };
            pair.first()
                .copied()
                .and_then(digit)
                .zip(pair.get(1).copied().and_then(digit))
                .map(|(a, b)| a * 16 + b)
                .ok_or_else(|| "Invalid raw storage hex".into())
        })
        .collect()
}

impl ProjectModel {
    pub fn from_metadata(metadata: &Value) -> Result<Self, String> {
        Self::from_metadata_with_cancel(metadata, || false)
    }
    pub fn from_metadata_with_cancel(metadata: &Value, mut cancelled: impl FnMut() -> bool) -> Result<Self, String> {
        if cancelled() {
            return Err("Project model preparation cancelled".into());
        }
        if metadata.get("schema_version").and_then(Value::as_u64) != Some(4) {
            return Err("Unsupported project metadata schema; version 4 is required".into());
        }
        let stored = metadata.get("tasks").and_then(Value::as_array).ok_or("Project tasks are missing")?;
        let resource_values = metadata.get("resources").and_then(Value::as_array);
        if stored.len() > LIMIT || resource_values.is_some_and(|r| r.len() > LIMIT) {
            return Err("Project record limit exceeded".into());
        }
        let mut errors = Vec::new();
        let calendars = match metadata.pointer("/raw_sections/Calendars/data_hex").and_then(Value::as_str) {
            Some(raw) => match decode_hex(raw, project_calendar::MAX_BYTES).and_then(|b| project_calendar::parse(&b).map_err(|e| e.to_string())) {
                Ok(c) => Some(c),
                Err(e) => {
                    errors.push(format!("Calendar data unresolved: {e}"));
                    None
                }
            },
            None => {
                errors.push("Calendar data is absent; planned finishes and working-time cost allocation remain unresolved".into());
                None
            }
        };
        let mut inputs = Vec::new();
        let mut index = BTreeMap::new();
        let mut relationships = 0usize;
        for value in stored {
            for field in ["assignments", "direct_costs"] {
                if value.get(field).and_then(Value::as_array).is_some_and(|items| items.len() > LIMIT) {
                    return Err("Task relationship collection exceeds the model budget".into());
                }
            }
            for field in ["name", "display_id"] {
                if let Some(label) = value.get(field).and_then(Value::as_str) {
                    check_label(label)?;
                }
            }
            // Borrow the JSON deserializer so ignored opaque fields are never cloned.
            let t = StoredTask::deserialize(value).map_err(|e| format!("Invalid task metadata: {e}"))?;
            identifier(&t.guid)?;
            check_label(&t.name)?;
            check_label(&t.display_id)?;
            if index.insert(t.guid.clone(), inputs.len()).is_some() {
                return Err("Duplicate task GUID".into());
            }
            relationships =
                relationships.checked_add(t.assignments.len()).and_then(|n| n.checked_add(t.direct_costs.len())).ok_or("Relationship count overflow")?;
            if relationships > LIMIT {
                return Err("Project relationship limit exceeded".into());
            }
            inputs.push(t);
        }
        let mut resources = Vec::new();
        let mut resource_index = BTreeMap::new();
        let mut cost_records = 0usize;
        if let Some(values) = resource_values {
            for value in values {
                let resource = parse_resource(value)?;
                cost_records = cost_records.checked_add(resource.rates.len()).ok_or("Cost record count overflow")?;
                for rate in &resource.rates {
                    cost_records = cost_records.checked_add(rate.items.len()).ok_or("Cost item count overflow")?;
                }
                if cost_records > LIMIT {
                    return Err("Aggregate resource cost record limit exceeded".into());
                }
                if resource_index.insert(resource.guid.clone(), resources.len()).is_some() {
                    return Err("Duplicate resource GUID".into());
                }
                resources.push(resource);
            }
        }
        let mut children = BTreeMap::<usize, Vec<usize>>::new();
        for (child, t) in inputs.iter().enumerate() {
            if let Some(parent) = t.parent_guid.as_ref().and_then(|p| index.get(p)) {
                children.entry(*parent).or_default().push(child);
            }
        }
        let mut tasks = Vec::new();
        let mut prepared = Vec::new();
        let mut total_segments = 0usize;
        for (i, stored) in inputs.iter().enumerate() {
            if cancelled() {
                return Err("Project model preparation cancelled".into());
            }
            let mut reasons = Vec::new();
            let calendar_result = resolve_calendar(i, &inputs, &index);
            let calendar_valid = calendar_result.is_ok();
            let calendar = match calendar_result {
                Ok(value) => value,
                Err(reason) => {
                    reasons.push(reason);
                    None
                }
            };
            let start = i64::try_from(stored.planned_start_wallclock_scalar).ok().and_then(valid_date);
            let schedule_kind = if children.contains_key(&i) {
                ScheduleKind::Summary
            } else if stored.duration_storage_scalar == 0 {
                ScheduleKind::Milestone
            } else {
                ScheduleKind::Work
            };
            let planned = match (calendars.as_ref().filter(|_| calendar_valid && schedule_kind == ScheduleKind::Work), start) {
                (Some(c), Some(s)) => match c.working_segments_with_budget(
                    calendar.as_deref(),
                    s,
                    stored.duration_storage_scalar,
                    project_calendar::WorkingBudget::default(),
                    &mut cancelled,
                ) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        reasons.push(format!("Planned working interval unresolved: {e}"));
                        None
                    }
                },
                _ => {
                    if schedule_kind == ScheduleKind::Work {
                        reasons.push("Planned working interval needs valid start and decoded calendar".into());
                    }
                    None
                }
            };
            total_segments = total_segments.checked_add(planned.as_ref().map_or(0, |p| p.segments.len())).ok_or("Working segment count overflow")?;
            if total_segments > SEGMENT_LIMIT {
                return Err("Project working-segment budget exceeded; reduce the task/date range".into());
            }
            let planned_start = if schedule_kind == ScheduleKind::Summary { None } else { start };
            let planned_finish = if schedule_kind == ScheduleKind::Milestone { start } else { planned.as_ref().map(|p| p.finish_wallclock) };
            let (best_start, best_finish) = if schedule_kind == ScheduleKind::Summary {
                (None, None)
            } else if schedule_kind == ScheduleKind::Milestone {
                let instant = stored.date_storage_windows.first().copied().and_then(valid_date).or(start);
                reasons.push("Zero-duration task is displayed as an instant; active appearance requires an explicit milestone policy".into());
                (instant, instant)
            } else {
                best_dates(stored, start, planned_finish, calendars.as_ref().filter(|_| calendar_valid), calendar.as_deref(), &mut reasons)
            };
            let mut policy_notes = vec!["Wall-clock values are not converted to UTC; calendar conventions are inferred from retained controls".into()];
            if nil(&stored.calendar_guid_storage) {
                policy_notes.push("Nil task calendar uses the declared nearest-parent then project-default policy".into());
            }
            tasks.push(TaskView {
                guid: stored.guid.clone(),
                parent_guid: stored.parent_guid.clone(),
                name: stored.name.clone(),
                display_id: stored.display_id.clone(),
                status_storage_value: stored.status_storage_value,
                schedule_kind,
                calendar_guid_storage: stored.calendar_guid_storage.clone(),
                effective_calendar_guid: calendar.or_else(|| calendars.as_ref().filter(|_| calendar_valid).and_then(|c| c.default_calendar_guid.clone())),
                date_storage_windows: stored.date_storage_windows,
                planned_start,
                planned_finish,
                best_start,
                best_finish,
                duration_storage_scalar: stored.duration_storage_scalar,
                assignment_count: stored.assignments.len(),
                declared_policy_notes: policy_notes,
                unresolved_reasons: reasons,
            });
            prepared.push(PreparedTask { stored: stored.clone(), planned });
        }
        let mut aggregate_state = vec![0u8; tasks.len()];
        for i in 0..tasks.len() {
            aggregate_task(i, 0, &children, &mut aggregate_state, &mut tasks)?;
        }
        let definitions = metadata.get("definition_storage").and_then(Value::as_object);
        let mut profiles = Vec::new();
        let mut profile_index = BTreeMap::new();
        if let Some(definitions) = definitions {
            if definitions.len() > LIMIT {
                return Err("Profile definition count exceeds the model budget".into());
            }
            for (guid, definition) in definitions {
                if definition.get("section").and_then(Value::as_str) != Some("Use Profiles") {
                    continue;
                }
                identifier(guid)?;
                let name = definition.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                check_label(&name)?;
                let raw = definition.get("raw_tail_hex").and_then(Value::as_str).and_then(|s| decode_hex(s, 1024).ok());
                let window = |offset| raw.as_ref().and_then(|b| b.get(offset..offset + 4)).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
                let base_kind_candidate = window(2);
                let direction_candidate = window(30);
                let opacity_percent_candidate = window(24);
                let supported_effect_signature = direction_candidate.is_none_or(|v| v == 1) && opacity_percent_candidate.is_none_or(|v| v == 100);
                let strict_profile = match name.as_str() {
                    "Install" if base_kind_candidate.is_none_or(|v| v == 1) && supported_effect_signature => DefaultProfile::Install,
                    "Maintain" if base_kind_candidate.is_none_or(|v| v == 4) && supported_effect_signature => DefaultProfile::Maintain,
                    _ => DefaultProfile::Unsupported,
                };
                let preview_profile =
                    if strict_profile == DefaultProfile::Unsupported && base_kind_candidate == Some(1) { DefaultProfile::Install } else { strict_profile };
                let unresolved_effects = if strict_profile == DefaultProfile::Unsupported && preview_profile == DefaultProfile::Install {
                    vec!["Declared Install interval preview omits directional growth, opacity/transparency and original-material effects; full native appearance remains unresolved".into()]
                } else {
                    Vec::new()
                };
                profile_index.insert(guid.clone(), profiles.len());
                profiles.push(ProfileView {
                    guid: guid.clone(),
                    name,
                    base_kind_candidate,
                    direction_candidate,
                    opacity_percent_candidate,
                    strict_profile,
                    preview_profile,
                    unresolved_effects,
                });
            }
        }
        let mut assignments = Vec::new();
        let mut preview_assignments = Vec::new();
        let mut resource_profile_effects = BTreeMap::<String, Vec<String>>::new();
        let mut lines = Vec::new();
        let mut direct_seen = BTreeSet::new();
        let mut derived_text_bytes = 0usize;
        for t in &tasks {
            charge_text(&mut derived_text_bytes, t.name.len() + t.display_id.len())?;
            for reason in t.unresolved_reasons.iter().chain(t.declared_policy_notes.iter()) {
                charge_text(&mut derived_text_bytes, reason.len())?;
            }
        }
        for r in &resources {
            charge_text(&mut derived_text_bytes, r.name.len() + 256)?;
            for reason in &r.unresolved_reasons {
                charge_text(&mut derived_text_bytes, reason.len())?;
            }
            for rate in &r.rates {
                for item in &rate.items {
                    charge_text(&mut derived_text_bytes, item.name.len())?;
                }
            }
        }
        for p in &profiles {
            charge_text(&mut derived_text_bytes, p.name.len())?;
        }
        for (task_index, t) in prepared.iter().enumerate() {
            if cancelled() {
                return Err("Project model preparation cancelled".into());
            }
            for a in &t.stored.assignments {
                if cancelled() {
                    return Err("Project model preparation cancelled".into());
                }
                identifier(&a.resource_guid)?;
                identifier(&a.profile_guid)?;
                let definition = profile_index.get(&a.profile_guid).and_then(|i| profiles.get(*i));
                let profile = definition.map_or(DefaultProfile::Unsupported, |p| p.strict_profile);
                assignments.push(Assignment {
                    task_id: t.stored.guid.clone(),
                    resource_id: a.resource_guid.clone(),
                    profile,
                    provenance: Provenance::CallerDeclared,
                });
                preview_assignments.push(Assignment {
                    task_id: t.stored.guid.clone(),
                    resource_id: a.resource_guid.clone(),
                    profile: definition.map_or(DefaultProfile::Unsupported, |p| p.preview_profile),
                    provenance: Provenance::CallerDeclared,
                });
                if let Some(profile) = definition {
                    let effects = resource_profile_effects.entry(a.resource_guid.clone()).or_default();
                    for effect in &profile.unresolved_effects {
                        if !effects.contains(effect) {
                            charge_text(&mut derived_text_bytes, effect.len())?;
                            effects.push(effect.clone());
                        }
                    }
                }
                let ri = resource_index.get(&a.resource_guid).copied();
                let units = millionths(a.planned_units).ok();
                let derived = t.stored.status_storage_value == 1 && (a.actual_units_storage.is_none() || a.remaining_units_storage.is_none());
                let actual = a.actual_units_storage.or_else(|| (t.stored.status_storage_value == 1).then_some(0.));
                let remaining = a.remaining_units_storage.or_else(|| (t.stored.status_storage_value == 1).then_some(a.planned_units));
                let mut line = CostLine {
                    task_guid: t.stored.guid.clone(),
                    task_name: t.stored.name.clone(),
                    resource_guid: Some(a.resource_guid.clone()),
                    resource_name: ri.and_then(|r| resources.get(r)).map(|r| r.name.clone()).unwrap_or_default(),
                    planned_units: Some(a.planned_units),
                    actual_units: actual,
                    remaining_units: remaining,
                    planned_status_units_derived: derived,
                    budget_minor: None,
                    actual_minor: None,
                    remaining_minor: None,
                    planned_cumulative_minor: None,
                    unresolved_reasons: Vec::new(),
                };
                match (ri.and_then(|r| resources.get(r)), t.planned.as_ref(), units) {
                    (Some(r), Some(p), Some(u)) => {
                        line.budget_minor = cost_for_work(r, p, u, None).map_err(|e| line.unresolved_reasons.push(e)).ok();
                        if let Some(amount) = constant_per_unit(r, p) {
                            line.actual_minor = actual.and_then(|v| millionths(v).ok()).and_then(|v| rounded(i128::from(amount) * i128::from(v), SCALE).ok());
                            line.remaining_minor =
                                remaining.and_then(|v| millionths(v).ok()).and_then(|v| rounded(i128::from(amount) * i128::from(v), SCALE).ok());
                        } else {
                            line.unresolved_reasons
                                .push("Actual/remaining cost requires a single effective Material Per Unit rate; other cases remain unresolved".into());
                        }
                    }
                    _ => line.unresolved_reasons.push("Cost requires a resource, valid units and resolved working calendar allocation".into()),
                }
                charge_line(&mut derived_text_bytes, &line)?;
                lines.push(PreparedLine { line, task_index, resource_index: ri, planned_units: units, fixed_direct: false });
            }
            for d in &t.stored.direct_costs {
                identifier(&d.guid)?;
                check_label(&d.name)?;
                if !direct_seen.insert((t.stored.guid.clone(), d.guid.clone())) {
                    return Err("Duplicate direct cost in one task".into());
                }
                let amount = (d.type_storage_value == 1).then(|| minor(d.amount_storage)).flatten();
                let mut reasons = vec!["Fixed task direct cost has no verified actual/remaining/time-phasing policy".into()];
                if amount.is_none() {
                    reasons.push("Unsupported direct cost type or amount".into());
                }
                let line = CostLine {
                    task_guid: t.stored.guid.clone(),
                    task_name: t.stored.name.clone(),
                    resource_guid: None,
                    resource_name: d.name.clone(),
                    planned_units: None,
                    actual_units: None,
                    remaining_units: None,
                    planned_status_units_derived: false,
                    budget_minor: amount,
                    actual_minor: None,
                    remaining_minor: None,
                    planned_cumulative_minor: None,
                    unresolved_reasons: reasons,
                };
                charge_line(&mut derived_text_bytes, &line)?;
                lines.push(PreparedLine { line, task_index, resource_index: None, planned_units: None, fixed_direct: true });
            }
        }
        if cancelled() {
            return Err("Project model preparation cancelled".into());
        }
        Ok(Self { tasks, resources, model_errors: errors, profiles, prepared, assignments, preview_assignments, resource_profile_effects, lines })
    }

    pub fn page_tasks(&self, offset: usize, limit: usize) -> Page<TaskView> {
        page(&self.tasks, offset, limit)
    }
    pub fn page_resources(&self, offset: usize, limit: usize) -> Page<ResourceCostView> {
        page(&self.resources, offset, limit)
    }
    pub fn evaluate(&self, focus: i64, date_mode: DateMode) -> Frame {
        // This wrapper cannot fail on cancellation; arithmetic failures remain explicit in lines.
        self.frame(focus, date_mode, AppearancePolicy::Strict, &mut || false)
    }
    pub fn evaluate_with_cancel(&self, focus: i64, date_mode: DateMode, mut cancelled: impl FnMut() -> bool) -> Result<Frame, String> {
        self.evaluate_with_policy(focus, date_mode, AppearancePolicy::Strict, &mut cancelled)
    }
    pub fn evaluate_with_policy(
        &self,
        focus: i64,
        date_mode: DateMode,
        appearance_policy: AppearancePolicy,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Frame, String> {
        let mut stopped = false;
        let frame = self.frame(focus, date_mode, appearance_policy, &mut || {
            stopped |= cancelled();
            stopped
        });
        if stopped { Err("Project evaluation cancelled".into()) } else { Ok(frame) }
    }
    fn frame(&self, focus: i64, date_mode: DateMode, appearance_policy: AppearancePolicy, cancelled: &mut impl FnMut() -> bool) -> Frame {
        let mut timeline = TimelineSummary::default();
        let mut unresolved = self.model_errors.clone();
        let mut simulation_tasks = Vec::new();
        for (i, t) in self.prepared.iter().enumerate() {
            let dates = self
                .tasks
                .get(i)
                .and_then(|v| match date_mode {
                    DateMode::Planned => v.planned_start.zip(v.planned_finish),
                    DateMode::Best => v.best_start.zip(v.best_finish),
                })
                .filter(|(s, e)| s <= e);
            let effective = dates.filter(|(s, e)| s < e).map(|(s, e)| Interval { start: WallClock(s), end: WallClock(e) });
            if let Some((start, end)) = dates {
                timeline.start = Some(timeline.start.map_or(start, |v| v.min(start)));
                timeline.end = Some(timeline.end.map_or(end, |v| v.max(end)));
                timeline.resolved_tasks += 1;
                if start == end {
                    timeline.milestone_tasks += 1;
                }
            } else {
                timeline.unresolved_tasks += 1;
            }
            simulation_tasks.push(sim::Task { id: t.stored.guid.clone(), effective, provenance: Provenance::CallerDeclared });
        }
        let mut resource_phases = Vec::new();
        let assignments = match appearance_policy {
            AppearancePolicy::Strict => &self.assignments,
            AppearancePolicy::IntervalPreview => &self.preview_assignments,
        };
        for r in &self.resources {
            if cancelled() {
                unresolved.push("Project evaluation cancelled".into());
                break;
            }
            let effects = self.resource_profile_effects.get(&r.guid).cloned().unwrap_or_default();
            match sim::evaluate_resource(&simulation_tasks, assignments, &r.guid, WallClock(focus)) {
                Ok(p) => {
                    let reason = match &p.state {
                        PhaseState::Unresolved { reason } => Some(reason.remediation().to_owned()),
                        _ => None,
                    };
                    resource_phases.push(ResourcePhase {
                        resource_guid: r.guid.clone(),
                        state: p.state,
                        provenance: p.provenance,
                        unresolved_reason: reason,
                        unresolved_profile_effects: effects,
                    });
                }
                Err(e) => resource_phases.push(ResourcePhase {
                    resource_guid: r.guid.clone(),
                    state: PhaseState::Unresolved { reason: sim::UnresolvedReason::UnsupportedProfileFamily },
                    provenance: Provenance::CallerDeclared,
                    unresolved_reason: Some(e.remediation().into()),
                    unresolved_profile_effects: effects,
                }),
            }
        }
        let mut costs=CostSummary{semantics:"Experimental declared linear utilization over decoded working segments; currency/unit conversion absent; nearest-cent inputs and final per-assignment rounding; partial totals exclude unresolved lines".into(),..Default::default()};
        for prepared in &self.lines {
            if cancelled() {
                unresolved.push("Project evaluation cancelled".into());
                break;
            }
            let mut line = prepared.line.clone();
            if !prepared.fixed_direct
                && let Some(((resource, task), units)) =
                    prepared.resource_index.and_then(|r| self.resources.get(r)).zip(self.prepared.get(prepared.task_index)).zip(prepared.planned_units)
                && let Some(work) = &task.planned
            {
                match cost_for_work(resource, work, units, Some(focus)) {
                    Ok(v) => line.planned_cumulative_minor = Some(v),
                    Err(e) => {
                        if !line.unresolved_reasons.contains(&e) {
                            line.unresolved_reasons.push(e);
                        }
                    }
                }
            }
            add_total(line.budget_minor, &mut costs.budget_minor, &mut costs.unresolved_budget_lines, &mut unresolved);
            if prepared.fixed_direct {
                add_total(line.budget_minor, &mut costs.direct_budget_minor, &mut costs.unresolved_direct_budget_lines, &mut unresolved);
            } else {
                add_total(line.budget_minor, &mut costs.resource_budget_minor, &mut costs.unresolved_resource_budget_lines, &mut unresolved);
            }
            add_total(line.actual_minor, &mut costs.actual_minor, &mut costs.unresolved_actual_lines, &mut unresolved);
            add_total(line.remaining_minor, &mut costs.remaining_minor, &mut costs.unresolved_remaining_lines, &mut unresolved);
            add_total(line.planned_cumulative_minor, &mut costs.planned_cumulative_minor, &mut costs.unresolved_cumulative_lines, &mut unresolved);
            costs.lines.push(line);
        }
        costs.complete = costs.unresolved_budget_lines == 0
            && costs.unresolved_actual_lines == 0
            && costs.unresolved_remaining_lines == 0
            && costs.unresolved_cumulative_lines == 0;
        Frame {
            schema_version: 1,
            focus,
            date_mode,
            appearance_policy,
            resource_phases,
            costs,
            timeline,
            unresolved_reasons: unresolved,
            native_parity_verified: false,
        }
    }
}

fn aggregate_task(index: usize, depth: usize, children: &BTreeMap<usize, Vec<usize>>, state: &mut [u8], tasks: &mut [TaskView]) -> Result<(), String> {
    if depth > 64 {
        return Err("Summary task hierarchy exceeds depth64".into());
    }
    match state.get(index).copied() {
        Some(2) => return Ok(()),
        Some(1) => return Err("Summary task parent cycle".into()),
        Some(_) => {}
        None => return Err("Summary task index missing".into()),
    }
    if let Some(value) = state.get_mut(index) {
        *value = 1;
    }
    if let Some(child_indices) = children.get(&index) {
        let mut planned = Some((i64::MAX, i64::MIN));
        let mut best = planned;
        for child in child_indices {
            aggregate_task(*child, depth + 1, children, state, tasks)?;
            let child = tasks.get(*child).ok_or("Summary child index missing")?;
            planned = planned.zip(child.planned_start.zip(child.planned_finish)).filter(|(_, (s, e))| s <= e).map(|((a, b), (s, e))| (a.min(s), b.max(e)));
            best = best.zip(child.best_start.zip(child.best_finish)).filter(|(_, (s, e))| s <= e).map(|((a, b), (s, e))| (a.min(s), b.max(e)));
        }
        let task = tasks.get_mut(index).ok_or("Summary task index missing")?;
        task.planned_start = planned.map(|p| p.0);
        task.planned_finish = planned.map(|p| p.1);
        task.best_start = best.map(|p| p.0);
        task.best_finish = best.map(|p| p.1);
        task.declared_policy_notes.push(
            "Summary dates aggregate all immediate children (minimum start, maximum finish); own raw stored start/duration are not schedule bounds".into(),
        );
        if planned.is_none() {
            task.unresolved_reasons.push("Summary planned dates unresolved because at least one child lacks resolved planned bounds".into());
        }
        if best.is_none() {
            task.unresolved_reasons.push("Summary Best dates unresolved because at least one child lacks resolved Best bounds".into());
        }
    }
    if let Some(value) = state.get_mut(index) {
        *value = 2;
    }
    Ok(())
}

fn page<T: Clone>(values: &[T], offset: usize, limit: usize) -> Page<T> {
    let start = offset.min(values.len());
    let items = values.iter().skip(start).take(limit.min(1000)).cloned().collect::<Vec<_>>();
    Page { offset: start, total: values.len(), more: start.saturating_add(items.len()) < values.len(), items }
}
fn add_total(value: Option<i64>, total: &mut i64, missing: &mut usize, reasons: &mut Vec<String>) {
    match value {
        Some(v) => match total.checked_add(v) {
            Some(next) => *total = next,
            None => {
                *missing += 1;
                if !reasons.iter().any(|s| s == "Cost total overflow; totals are partial") {
                    reasons.push("Cost total overflow; totals are partial".into());
                }
            }
        },
        None => *missing += 1,
    }
}
fn resolve_calendar(i: usize, tasks: &[StoredTask], index: &BTreeMap<String, usize>) -> Result<Option<String>, String> {
    let mut current = Some(i);
    let mut seen = BTreeSet::new();
    for _ in 0..65 {
        let Some(position) = current else { return Ok(None) };
        if !seen.insert(position) {
            return Err("Task parent cycle prevents calendar inheritance".into());
        }
        let t = tasks.get(position).ok_or("Task index missing during calendar inheritance")?;
        if !nil(&t.calendar_guid_storage) {
            return Ok(Some(t.calendar_guid_storage.clone()));
        }
        current = t.parent_guid.as_ref().and_then(|p| index.get(p)).copied();
        if t.parent_guid.is_some() && current.is_none() {
            return Err("Task parent missing; calendar inheritance unresolved".into());
        }
    }
    Err("Task calendar inheritance exceeds depth64".into())
}
fn best_dates(
    t: &StoredTask,
    planned_start: Option<i64>,
    planned_finish: Option<i64>,
    calendars: Option<&Calendars>,
    calendar: Option<&str>,
    reasons: &mut Vec<String>,
) -> (Option<i64>, Option<i64>) {
    let actual = t.date_storage_windows.first().copied().and_then(valid_date);
    let estimated = t.date_storage_windows.get(2).copied().and_then(valid_date);
    let actual_finish = t.date_storage_windows.get(3).copied().and_then(valid_date);
    let start = actual.or(planned_start);
    let finish = match t.status_storage_value {
        1 => planned_finish,
        3 => actual_finish.or_else(|| {
            reasons.push("Finished task lacks Actual Finish; finish remains unresolved".into());
            None
        }),
        2 => estimated.or_else(|| {
            reasons.push(
                "Started task lacks Estimated Finish; declared fallback uses full stored planned duration from selected start, not inferred remaining duration"
                    .into(),
            );
            start.zip(calendars).and_then(|(s, c)| c.working_segments(calendar, s, t.duration_storage_scalar).ok()).map(|p| p.finish_wallclock)
        }),
        _ => {
            reasons.push("Unsupported task status; Best dates unresolved".into());
            None
        }
    };
    (start, finish)
}
fn parse_resource(value: &Value) -> Result<ResourceCostView, String> {
    let guid = value.get("guid").and_then(Value::as_str).ok_or("Resource GUID missing")?.to_owned();
    identifier(&guid)?;
    let name = value.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    check_label(&name)?;
    let kind = value.get("kind_storage_byte").and_then(Value::as_u64).and_then(|v| u8::try_from(v).ok());
    let unit_storage_byte = value.get("unit_storage_byte").and_then(Value::as_u64).and_then(|v| u8::try_from(v).ok());
    let mut reasons = Vec::new();
    if kind != Some(4) {
        reasons.push("Cost formulas are admitted only for Material kind4; other resource kinds remain unresolved".into());
    }
    let groups = value.get("cost_storage_groups").and_then(Value::as_array);
    if groups.is_some_and(|v| v.len() > LIMIT) {
        return Err("Resource rate group limit exceeded".into());
    }
    let mut rates = Vec::new();
    if let Some(groups) = groups {
        for group in groups {
            let effective = group
                .get("raw_header_hex")
                .and_then(Value::as_str)
                .and_then(|raw| decode_hex(raw, 256).ok())
                .and_then(|bytes| bytes.get(28..36).and_then(|s| s.try_into().ok()).map(i64::from_le_bytes))
                .and_then(valid_date);
            if effective.is_none() {
                reasons.push("A rate group has unresolved effective date storage".into());
            }
            let source = group.get("items").and_then(Value::as_array).ok_or("Resource rate items missing")?;
            if source.len() > LIMIT {
                return Err("Resource cost item limit exceeded".into());
            }
            let mut items = Vec::new();
            for item in source {
                let amount = item.get("amount_storage").and_then(Value::as_f64).ok_or("Resource amount missing")?;
                let name = item.get("description").and_then(Value::as_str).unwrap_or_default().to_owned();
                check_label(&name)?;
                items.push(CostItemView {
                    name,
                    amount_storage: amount,
                    amount_minor: minor(amount),
                    type_storage: item.get("type_storage").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).unwrap_or_default(),
                });
            }
            rates.push(RateView { effective_wallclock: effective, items });
        }
    }
    rates.sort_by_key(|r| r.effective_wallclock);
    if rates.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| a.effective_wallclock == b.effective_wallclock)) {
        reasons.push("Duplicate effective rate dates are ambiguous".into());
    }
    Ok(ResourceCostView { guid, name, kind_storage_byte: kind, unit_storage_byte, rates, unresolved_reasons: reasons })
}
fn admitted_rates(resource: &ResourceCostView) -> Result<Vec<(i64, i64, u32)>, String> {
    if resource.kind_storage_byte != Some(4) {
        return Err("Unsupported resource kind for Material cost formula".into());
    }
    if resource.rates.is_empty() {
        return Err("Resource has no decoded effective cost rates".into());
    }
    let mut out = Vec::new();
    let mut previous = None;
    for rate in &resource.rates {
        let date = rate.effective_wallclock.ok_or("Rate effective date unresolved")?;
        if previous == Some(date) {
            return Err("Duplicate rate dates are ambiguous".into());
        }
        previous = Some(date);
        if rate.items.len() != 1 {
            return Err("Only singleton cost items per effective date are admitted".into());
        }
        let item = rate.items.first().ok_or("Missing cost item")?;
        let amount = item.amount_minor.ok_or("Invalid monetary amount")?;
        if ![1, 9].contains(&item.type_storage) {
            return Err("Unsupported resource monetary cost type".into());
        }
        out.push((date, amount, item.type_storage));
    }
    Ok(out)
}
fn constant_per_unit(resource: &ResourceCostView, work: &WorkAllocation) -> Option<i64> {
    let rates = admitted_rates(resource).ok()?;
    let first = rates.first()?;
    (rates.len() == 1 && first.2 == 9 && first.0 <= work.requested_start_wallclock).then_some(first.1)
}
fn cost_for_work(resource: &ResourceCostView, work: &WorkAllocation, units: u64, focus: Option<i64>) -> Result<i64, String> {
    let rates = admitted_rates(resource)?;
    if work.segments.len().checked_mul(rates.len()).is_none_or(|n| n > 1_000_000) {
        return Err("Working allocation/rate lookup budget exceeded".into());
    }
    if work.requested_working_seconds == 0 {
        return Err("Zero working duration needs an explicit cost allocation policy".into());
    }
    if rates.iter().any(|r| r.2 == 1) {
        if focus.is_some() {
            return Err("Fixed budget has no verified cumulative time-phasing policy".into());
        }
        if rates.iter().any(|r| r.2 != 1) {
            return Err("Mixed Fixed/Per Unit rate windows need an explicit policy".into());
        }
        return Ok(rates.iter().rev().find(|r| r.0 <= work.requested_start_wallclock).map_or(0, |r| r.1));
    }
    let mut numerator = 0i128;
    for segment in &work.segments {
        let end = focus.map_or(segment.end_wallclock, |f| f.min(segment.end_wallclock));
        let mut cursor = segment.start_wallclock;
        if end <= cursor {
            continue;
        }
        while cursor < end {
            let active = rates.iter().rev().find(|r| r.0 <= cursor);
            let boundary = rates.iter().find(|r| r.0 > cursor).map_or(end, |r| end.min(r.0));
            let seconds = boundary.checked_sub(cursor).ok_or("Cost interval overflow")?;
            let term = i128::from(active.map_or(0, |r| r.1))
                .checked_mul(i128::from(units))
                .and_then(|v| v.checked_mul(i128::from(seconds)))
                .ok_or("Cost allocation overflow")?;
            numerator = numerator.checked_add(term).ok_or("Cost allocation sum overflow")?;
            cursor = boundary;
        }
    }
    rounded(numerator, SCALE.checked_mul(i128::from(work.requested_working_seconds)).ok_or("Cost denominator overflow")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn late_preparation_cancellation_never_returns_a_cacheable_model() {
        let data = json!({"schema_version":4,"tasks":[],"resources":[],"definition_storage":{}});
        let mut calls = 0;
        let result = ProjectModel::from_metadata_with_cancel(&data, || {
            calls += 1;
            calls > 1
        });
        assert!(result.is_err());
        assert!(calls > 1);
        assert!(ProjectModel::from_metadata_with_cancel(&data, || true).is_err());
    }
    fn metadata() -> Value {
        json!({"schema_version":4,"tasks":[{"guid":"task-a","name":"Assembly","planned_start_wallclock_scalar":100,"duration_storage_scalar":20,"status_storage_value":2,"date_storage_windows":[200,999,230,UNSET],"assignments":[]}],"resources":[],"definition_storage":{}})
    }
    #[test]
    fn missing_calendars_preserves_inventory_and_best_dates() {
        let model = ProjectModel::from_metadata(&metadata()).unwrap();
        assert_eq!(model.tasks.len(), 1);
        assert!(model.tasks[0].planned_finish.is_none());
        let frame = model.evaluate(210, DateMode::Best);
        assert_eq!((frame.timeline.start, frame.timeline.end), (Some(200), Some(230)));
        assert!(!frame.native_parity_verified);
        assert!(!model.model_errors.is_empty());
    }
    #[test]
    fn planned_dates_never_add_elapsed_duration() {
        let model = ProjectModel::from_metadata(&metadata()).unwrap();
        let frame = model.evaluate(110, DateMode::Planned);
        assert_eq!(frame.timeline.end, None);
        assert_eq!(frame.timeline.unresolved_tasks, 1);
    }
    #[test]
    fn pagination_and_bad_schema_are_bounded() {
        let model = ProjectModel::from_metadata(&metadata()).unwrap();
        assert_eq!(model.page_tasks(100, 10000).items.len(), 0);
        assert_eq!(model.page_tasks(0, 1).total, 1);
        assert!(ProjectModel::from_metadata(&json!({"schema_version":3})).is_err());
    }
    #[test]
    fn scalar_conversions_reject_unknown_and_round_half_up() {
        assert_eq!(valid_date(UNSET), None);
        assert_eq!(minor(1.235), Some(124));
        assert_eq!(minor(-1.), None);
        assert!(millionths(f64::NAN).is_err());
        assert_eq!(rounded(5, 2), Ok(3));
    }
    fn work() -> WorkAllocation {
        WorkAllocation {
            calendar_guid: "cal".into(),
            requested_start_wallclock: 0,
            first_work_start_wallclock: Some(0),
            finish_wallclock: 30,
            requested_working_seconds: 20,
            segments: vec![
                project_calendar::WorkSegment { start_wallclock: 0, end_wallclock: 10, working_seconds: 10, day_type_guid: "day".into() },
                project_calendar::WorkSegment { start_wallclock: 20, end_wallclock: 30, working_seconds: 10, day_type_guid: "day".into() },
            ],
            inferred_storage_conventions_used: true,
        }
    }
    fn resource() -> ResourceCostView {
        ResourceCostView {
            guid: "resource".into(),
            name: "Material".into(),
            kind_storage_byte: Some(4),
            unit_storage_byte: None,
            rates: vec![
                RateView {
                    effective_wallclock: Some(-1),
                    items: vec![CostItemView { name: "rate".into(), amount_storage: 1., amount_minor: Some(100), type_storage: 9 }],
                },
                RateView {
                    effective_wallclock: Some(20),
                    items: vec![CostItemView { name: "rate".into(), amount_storage: 2., amount_minor: Some(200), type_storage: 9 }],
                },
            ],
            unresolved_reasons: vec![],
        }
    }
    #[test]
    fn working_rate_crossing_and_focus_cumulative_exclude_nonworking_gap() {
        let r = resource();
        let w = work();
        assert_eq!(cost_for_work(&r, &w, 2_000_000, None), Ok(300));
        assert_eq!(cost_for_work(&r, &w, 2_000_000, Some(15)), Ok(100));
        assert_eq!(cost_for_work(&r, &w, 2_000_000, Some(25)), Ok(200));
        assert_eq!(cost_for_work(&r, &w, 2_000_000, Some(-1)), Ok(0));
    }
    #[test]
    fn fixed_and_nonmaterial_costs_do_not_invent_actual_or_timephase() {
        let mut r = resource();
        r.rates.truncate(1);
        r.rates[0].items[0].type_storage = 1;
        assert_eq!(cost_for_work(&r, &work(), 9_000_000, None), Ok(100));
        assert!(cost_for_work(&r, &work(), 9_000_000, Some(10)).is_err());
        assert_eq!(constant_per_unit(&r, &work()), None);
        r.kind_storage_byte = Some(1);
        assert!(cost_for_work(&r, &work(), 1, None).is_err());
    }
    #[test]
    fn structured_frame_roundtrips() {
        let frame = ProjectModel::from_metadata(&metadata()).unwrap().evaluate(210, DateMode::Best);
        let encoded = serde_json::to_value(frame).unwrap();
        assert_eq!(serde_json::from_value::<Frame>(encoded).unwrap().date_mode, DateMode::Best);
    }
    #[test]
    fn direct_budgets_are_counted_once_separately_from_resource_forecasts() {
        let mut data = metadata();
        data["tasks"][0]["direct_costs"] = json!([{"guid":"expense-one","name":"Setup","amount_storage":1.11,"type_storage_value":1},{"guid":"expense-two","name":"Delivery","amount_storage":2.22,"type_storage_value":1}]);
        let frame = ProjectModel::from_metadata(&data).unwrap().evaluate(210, DateMode::Best);
        assert_eq!(frame.costs.direct_budget_minor, 333);
        assert_eq!(frame.costs.resource_budget_minor, 0);
        assert_eq!(frame.costs.budget_minor, 333);
        assert_eq!(frame.costs.unresolved_direct_budget_lines, 0);
        assert_eq!(frame.costs.unresolved_actual_lines, 2);
        assert!(!frame.costs.complete);
        assert_eq!(frame.page_costs(0, 1).items.len(), 1);
        assert!(frame.page_costs(0, 1).more);
    }
    #[test]
    fn retained_profile_names_map_without_interpreting_assignment_identity() {
        let mut data = metadata();
        data["tasks"][0]["assignments"] =
            json!([{"resource_guid":"part","profile_guid":"default-profile","planned_units":2.,"identity_candidate":"opaque-not-an-id"}]);
        data["resources"] = json!([{"guid":"part","name":"Panel","kind_storage_byte":4,"cost_storage_groups":[]}]);
        data["definition_storage"] = json!({"default-profile":{"section":"Use Profiles","name":"Install"}});
        let frame = ProjectModel::from_metadata(&data).unwrap().evaluate(210, DateMode::Best);
        assert!(matches!(frame.resource_phases[0].state, PhaseState::Resolved { phase: sim::Phase::InstallActive, .. }));
        assert_eq!(frame.costs.unresolved_budget_lines, 1);
        assert_eq!(frame.costs.lines[0].budget_minor, None);
    }
    #[test]
    fn summary_ignores_dummy_storage_and_aggregates_all_child_bounds() {
        let mut data = metadata();
        let mut child = data["tasks"][0].clone();
        child["guid"] = json!("child");
        child["parent_guid"] = json!("task-a");
        child["date_storage_windows"] = json!([400, 999, 460, UNSET]);
        data["tasks"].as_array_mut().unwrap().push(child);
        let model = ProjectModel::from_metadata(&data).unwrap();
        let root = &model.tasks[0];
        assert_eq!(root.schedule_kind, ScheduleKind::Summary);
        assert_eq!((root.best_start, root.best_finish), (Some(400), Some(460)));
        assert_eq!(root.planned_start, None);
        assert!(root.unresolved_reasons.iter().any(|r| r.contains("at least one child")));
        let frame = model.evaluate(410, DateMode::Best);
        assert_eq!((frame.timeline.start, frame.timeline.end), (Some(400), Some(460)));
    }
    #[test]
    fn incomplete_summary_children_propagate_unresolved_instead_of_partial_bounds() {
        let mut data = metadata();
        let mut child = data["tasks"][0].clone();
        child["guid"] = json!("child");
        child["parent_guid"] = json!("task-a");
        child["status_storage_value"] = json!(3);
        child["date_storage_windows"] = json!([400, 999, UNSET, UNSET]);
        data["tasks"].as_array_mut().unwrap().push(child);
        let model = ProjectModel::from_metadata(&data).unwrap();
        assert_eq!(model.tasks[0].best_finish, None);
    }
    #[test]
    fn zero_duration_is_displayed_as_milestone_without_active_interval() {
        let mut data = metadata();
        data["tasks"][0]["duration_storage_scalar"] = json!(0);
        data["tasks"][0]["assignments"] = json!([{"resource_guid":"part","profile_guid":"p","planned_units":1.}]);
        data["resources"] = json!([{"guid":"part","name":"marker","kind_storage_byte":4,"cost_storage_groups":[]}]);
        data["definition_storage"] = json!({"p":{"section":"Use Profiles","name":"Install"}});
        let model = ProjectModel::from_metadata(&data).unwrap();
        let frame = model.evaluate(200, DateMode::Best);
        assert_eq!(model.tasks[0].schedule_kind, ScheduleKind::Milestone);
        assert_eq!(frame.timeline.milestone_tasks, 1);
        assert_eq!((frame.timeline.start, frame.timeline.end), (Some(200), Some(200)));
        assert!(matches!(frame.resource_phases[0].state, PhaseState::Unresolved { .. }));
    }
    #[test]
    fn arbitrary_named_install_candidate_requires_explicit_preview_and_preserves_effects() {
        let mut data = metadata();
        let mut tail = vec![0u8; 247];
        tail[2..6].copy_from_slice(&1u32.to_le_bytes());
        tail[24..28].copy_from_slice(&55u32.to_le_bytes());
        tail[30..34].copy_from_slice(&3u32.to_le_bytes());
        let hex = tail.iter().map(|b| format!("{b:02x}")).collect::<String>();
        data["tasks"][0]["assignments"] = json!([{"resource_guid":"part","profile_guid":"profile","planned_units":1.}]);
        data["resources"] = json!([{"guid":"part","name":"Piece","kind_storage_byte":4,"cost_storage_groups":[]}]);
        data["definition_storage"] = json!({"profile":{"section":"Use Profiles","name":"Custom construction","raw_tail_hex":hex}});
        let model = ProjectModel::from_metadata(&data).unwrap();
        let strict = model.evaluate(210, DateMode::Best);
        assert_eq!(strict.appearance_policy, AppearancePolicy::Strict);
        assert!(matches!(strict.resource_phases[0].state, PhaseState::Unresolved { .. }));
        let preview = model.evaluate_with_policy(210, DateMode::Best, AppearancePolicy::IntervalPreview, || false).unwrap();
        assert!(matches!(preview.resource_phases[0].state, PhaseState::Resolved { phase: sim::Phase::InstallActive, .. }));
        assert!(!preview.resource_phases[0].unresolved_profile_effects.is_empty());
        assert!(!preview.native_parity_verified);
        assert_eq!(model.profiles[0].base_kind_candidate, Some(1));
        assert_eq!(model.profiles[0].opacity_percent_candidate, Some(55));
        assert_eq!(model.profiles[0].direction_candidate, Some(3));
    }
    #[test]
    fn oversized_labels_and_amplified_costline_text_are_rejected() {
        let mut data = metadata();
        data["tasks"][0]["name"] = json!("x".repeat(MAX_LABEL_BYTES + 1));
        assert!(ProjectModel::from_metadata(&data).unwrap_err().contains("4096"));
        data["tasks"][0]["name"] = json!("x".repeat(MAX_LABEL_BYTES));
        data["tasks"][0]["assignments"] = Value::Array((0..1200).map(|_| json!({"resource_guid":"part","profile_guid":"p","planned_units":1.})).collect());
        data["resources"] = json!([{"guid":"part","name":"Piece","kind_storage_byte":4,"cost_storage_groups":[]}]);
        assert!(ProjectModel::from_metadata(&data).unwrap_err().contains("4 MiB"));
    }
    #[test]
    fn builtin_label_cannot_override_conflicting_raw_kind_or_effects() {
        for (kind, direction, opacity) in [(3u32, 1u32, 100u32), (1, 2, 100), (1, 1, 60)] {
            let mut data = metadata();
            let mut tail = vec![0u8; 247];
            tail[2..6].copy_from_slice(&kind.to_le_bytes());
            tail[24..28].copy_from_slice(&opacity.to_le_bytes());
            tail[30..34].copy_from_slice(&direction.to_le_bytes());
            let hex = tail.iter().map(|b| format!("{b:02x}")).collect::<String>();
            data["definition_storage"] = json!({"profile":{"section":"Use Profiles","name":"Install","raw_tail_hex":hex}});
            let model = ProjectModel::from_metadata(&data).unwrap();
            assert_eq!(model.profiles[0].strict_profile, DefaultProfile::Unsupported);
            assert_eq!(model.profiles[0].preview_profile, if kind == 1 { DefaultProfile::Install } else { DefaultProfile::Unsupported });
        }
    }
}
