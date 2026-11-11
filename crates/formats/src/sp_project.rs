//! Bounded experimental importer for the observed 6.5.5.28 project layout.
//! Pure memory parsing; schedule and cost values retain raw storage semantics.
mod definitions;
mod resource_tree;
mod scene_geometry;
mod scene_graph;
mod scene_io;
use crate::{FormatError, scene_bundle};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
const FILE_LIMIT: usize = 256 * 1024 * 1024;
const STREAM_LIMIT: usize = 16 * 1024 * 1024;
const TOTAL_LIMIT: usize = 64 * 1024 * 1024;
const RECORD_LIMIT: usize = 10_000;
const OPEN: &[u8] = &[0x7a, 0x54, 0xaf, 0x39];
const CLOSE: &[u8] = &[0x16, 0xe4, 0x99, 0x6f];
type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Serialize)]
struct Failure {
    code: &'static str,
    message: String,
    section: Option<String>,
    offset: Option<usize>,
}
fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure { code, message: message.into(), section: None, offset: None }
}
fn checked_add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or_else(|| fail("limit", "Offset arithmetic exceeds limit"))
}
fn bytes(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    data.get(at..checked_add(at, len)?).ok_or_else(|| Failure {
        code: "truncated",
        message: "Required byte range is missing".into(),
        section: None,
        offset: Some(at),
    })
}
fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, at, 4)?.try_into().map_err(|_| fail("truncated", "Missing u32"))?))
}
fn u64_at(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, at, 8)?.try_into().map_err(|_| fail("truncated", "Missing u64"))?))
}
fn count_at(data: &[u8], at: usize) -> Result<usize> {
    let n = usize::try_from(u32_at(data, at)?).map_err(|_| fail("limit", "Count does not fit address space"))?;
    if n > RECORD_LIMIT {
        return Err(fail("limit", "Record count exceeds 10000"));
    }
    Ok(n)
}
fn marker(data: &[u8], at: usize, expected: &[u8]) -> Result<()> {
    if bytes(data, at, expected.len())? != expected {
        return Err(Failure {
            code: "unsupported_layout",
            message: "Unexpected record marker; this layout is not admitted".into(),
            section: None,
            offset: Some(at),
        });
    }
    Ok(())
}
fn string_at(data: &[u8], at: usize) -> Result<(String, usize)> {
    marker(data, at, &[255, 254, 255])?;
    let n = usize::from(*bytes(data, checked_add(at, 3)?, 1)?.first().ok_or_else(|| fail("truncated", "Missing string count"))?);
    let (n, start) = if n == 255 {
        let p = checked_add(at, 4)?;
        let n = u16::from_le_bytes(bytes(data, p, 2)?.try_into().map_err(|_| fail("truncated", "Missing long string count"))?);
        (usize::from(n), checked_add(at, 6)?)
    } else {
        (n, checked_add(at, 4)?)
    };
    if n > 16_384 {
        return Err(fail("limit", "String exceeds 16384 UTF-16 units"));
    }
    let len = n.checked_mul(2).ok_or_else(|| fail("limit", "String length overflow"))?;
    let units = bytes(data, start, len)?.as_chunks::<2>().0.iter().map(|v| u16::from_le_bytes(*v)).collect::<Vec<_>>();
    let value = String::from_utf16(&units).map_err(|_| fail("invalid_string", "Invalid UTF-16 string"))?;
    Ok((value, checked_add(start, len)?))
}
fn guid_at(data: &[u8], at: usize) -> Result<String> {
    let a = u32_at(data, at)?;
    let b = u16::from_le_bytes(bytes(data, checked_add(at, 4)?, 2)?.try_into().map_err(|_| fail("truncated", "GUID"))?);
    let c = u16::from_le_bytes(bytes(data, checked_add(at, 6)?, 2)?.try_into().map_err(|_| fail("truncated", "GUID"))?);
    let suffix = bytes(data, checked_add(at, 8)?, 8)?;
    let hex = suffix.iter().map(|v| format!("{v:02x}")).collect::<String>();
    Ok(format!("{a:08x}-{b:04x}-{c:04x}-{}-{}", hex.get(..4).ok_or_else(|| fail("truncated", "GUID"))?, hex.get(4..).ok_or_else(|| fail("truncated", "GUID"))?))
}
fn finite_at(data: &[u8], at: usize) -> Result<f64> {
    let v = f64::from_bits(u64_at(data, at)?);
    if !v.is_finite() {
        return Err(fail("unsupported_value", "Required numeric field is not finite"));
    }
    Ok(v)
}
#[derive(Serialize)]
struct Section {
    file_offset: usize,
    stored_bytes: usize,
    decoded_bytes: Option<usize>,
    sha256: String,
    encoding: &'static str,
}
struct Container {
    sections: BTreeMap<String, Section>,
    decoded: BTreeMap<String, Vec<u8>>,
}
#[derive(Serialize)]
struct Assignment {
    offset: usize,
    bytes: usize,
    resource_guid: String,
    profile_guid: String,
    identity_candidate: String,
    raw_record_hex: String,
    planned_units: f64,
    rate_storage_window: f64,
    remaining_units_storage: Option<f64>,
    remaining_rate_storage: Option<f64>,
    actual_units_storage: Option<f64>,
}
#[derive(Serialize)]
struct Link {
    offset: usize,
    predecessor_guid: String,
    type_storage_window: u32,
    lag_storage_hex: String,
}

#[derive(Serialize)]
struct ProgressRecord {
    offset: usize,
    bytes: usize,
    opaque_guid: String,
    date_storage_scalar: u64,
    duration_storage_scalar: u64,
    type_storage_value: u32,
}
// Admitted only: observed optional list with 88-byte records, empty label/nested list.
fn progress_records(data: &[u8], at: usize, budget: usize) -> Result<(Vec<ProgressRecord>, usize)> {
    let flag = *bytes(data, at, 1)?.first().ok_or_else(|| fail("truncated", "Missing progress flag"))?;
    if flag == 0 {
        return Ok((Vec::new(), 0));
    }
    if flag != 1 {
        return Err(Failure {
            code: "unsupported_layout",
            message: "Unknown progress list flag; earlier record-layout divergence is also possible".into(),
            section: None,
            offset: Some(at),
        });
    }
    let open = checked_add(at, 1)?;
    marker(data, open, OPEN)?;
    let count = count_at(data, checked_add(open, 4)?)?;
    if count > budget {
        return Err(fail("limit", "Total relationships/progress exceeds 10000"));
    }
    let mut cursor = checked_add(open, 8)?;
    let mut result = Vec::new();
    for _ in 0..count {
        let record = bytes(data, cursor, 88)?;
        marker(record, 0, &[0x32, 0xe6, 0xb5, 0x72])?;
        marker(record, 64, &[255, 254, 255, 0])?;
        marker(record, 68, OPEN)?;
        if u32_at(record, 72)? != 0 {
            return Err(fail("unsupported_layout", "Nested progress lists are outside this reader"));
        }
        marker(record, 76, CLOSE)?;
        marker(record, 80, &[0xce, 0xac, 0x72, 0xc9])?;
        marker(record, 84, &[0; 4])?;
        result.push(ProgressRecord {
            offset: cursor,
            bytes: 88,
            opaque_guid: guid_at(record, 12)?,
            date_storage_scalar: u64_at(record, 44)?,
            duration_storage_scalar: u64_at(record, 52)?,
            type_storage_value: u32_at(record, 60)?,
        });
        cursor = checked_add(cursor, 88)?;
    }
    marker(data, cursor, CLOSE)?;
    let end = checked_add(cursor, 4)?;
    let extra = end.checked_sub(open).ok_or_else(|| fail("limit", "Progress length underflow"))?;
    Ok((result, extra))
}

#[derive(Serialize)]
struct Task {
    raw_header_hex: String,
    calendar_guid_storage: String,
    date_storage_windows: [i64; 4],
    raw_dynamic_window_hex: String,
    offset: usize,
    bytes: usize,
    guid: String,
    parent_guid: Option<String>,
    name: String,
    display_id: String,
    name_offset: usize,
    name_end: usize,
    planned_start_wallclock_scalar: u64,
    duration_storage_scalar: u64,
    status_storage_value: u32,
    progress_records: Vec<ProgressRecord>,
    assignments: Vec<Assignment>,
    links: Vec<Link>,
    direct_costs: Vec<DirectCost>,
    activity_code_refs: Vec<CodeReference>,
    user_values: Vec<UserValue>,
}
#[derive(Serialize)]
struct DirectCost {
    offset: usize,
    guid: String,
    name: String,
    amount_storage: f64,
    type_storage_value: u32,
}
#[derive(Serialize)]
struct CodeReference {
    offset: usize,
    guid: String,
    ordinal_storage: u32,
}
#[derive(Serialize)]
struct UserValue {
    offset: usize,
    variant: u32,
    reference_offset: usize,
    definition_guid: String,
    integer_storage: Option<i32>,
    number_storage: Option<f64>,
    text_storage: Option<String>,
    opaque_tail_hex: String,
    ordinal_storage: u32,
}
fn direct_costs(data: &[u8], at: usize, budget: usize) -> Result<(Vec<DirectCost>, usize)> {
    let flag = bytes(data, at, 1)?;
    if flag == [0] {
        return Ok((Vec::new(), 0));
    }
    marker(data, at, &[1])?;
    marker(data, checked_add(at, 1)?, OPEN)?;
    let n = count_at(data, checked_add(at, 5)?)?;
    if n > budget {
        return Err(fail("limit", "Direct-cost budget exceeded"));
    }
    if n != 1 {
        return Err(fail("unsupported_layout", "Only observed singleton direct-cost lists admitted"));
    }
    let start = checked_add(at, 9)?;
    marker(data, start, &[0x9c, 0x0e, 0xe3, 0x04])?;
    marker(data, checked_add(start, 4)?, &[0; 8])?;
    let (name, end) = string_at(data, checked_add(start, 28)?)?;
    marker(data, checked_add(end, 8)?, &[1, 0, 0, 0])?;
    marker(data, checked_add(end, 12)?, &[0; 16])?;
    marker(data, checked_add(end, 28)?, &[0x78, 0xe2, 0x44, 0xa1, 0, 0, 0, 0])?;
    let close = checked_add(end, 36)?;
    marker(data, close, CLOSE)?;
    Ok((
        vec![DirectCost { offset: start, guid: guid_at(data, checked_add(start, 12)?)?, name, amount_storage: finite_at(data, end)?, type_storage_value: 1 }],
        checked_add(close, 4)?.checked_sub(checked_add(at, 1)?).ok_or_else(|| fail("limit", "Cost size"))?,
    ))
}
fn code_references(data: &[u8], at: usize, budget: usize) -> Result<(Vec<CodeReference>, usize)> {
    marker(data, at, &[0xd0, 0xbc, 0xa1, 0x06])?;
    let n = count_at(data, checked_add(at, 4)?)?;
    if n > budget {
        return Err(fail("limit", "Code reference budget exceeded"));
    }
    let mut pos = checked_add(at, 8)?;
    let mut rows = Vec::new();
    for _ in 0..n {
        rows.push(CodeReference { offset: pos, guid: guid_at(data, pos)?, ordinal_storage: u32_at(data, checked_add(pos, 16)?)? });
        pos = checked_add(pos, 20)?;
    }
    marker(data, pos, &[0x2c, 0x57, 0xd9, 0xda])?;
    Ok((rows, checked_add(pos, 4)?))
}
fn user_values(data: &[u8], at: usize, budget: usize) -> Result<(Vec<UserValue>, usize)> {
    if bytes(data, at, 1)? == [0] {
        return Ok((Vec::new(), 0));
    }
    marker(data, at, &[1])?;
    marker(data, checked_add(at, 1)?, OPEN)?;
    let n = count_at(data, checked_add(at, 5)?)?;
    if n > budget {
        return Err(fail("limit", "User value budget exceeded"));
    }
    let mut pos = checked_add(at, 9)?;
    let mut rows = Vec::new();
    for _ in 0..n {
        let start = pos;
        marker(data, pos, &[0xa3, 0x7b, 0x0a, 0x44])?;
        let variant = u32_at(data, checked_add(pos, 4)?)?;
        let value_at = checked_add(pos, 8)?;
        let (integer_storage, number_storage, text_storage, reference_offset) = match variant {
            3 => (
                Some(i32::from_le_bytes(bytes(data, value_at, 4)?.try_into().map_err(|_| fail("truncated", "Integer value"))?)),
                None,
                None,
                checked_add(value_at, 4)?,
            ),
            4 => (None, Some(finite_at(data, value_at)?), None, checked_add(value_at, 8)?),
            6 => {
                let (value, end) = string_at(data, value_at)?;
                (None, None, Some(value), end)
            }
            _ => return Err(Failure { code: "unsupported_layout", message: "Unknown user-value variant".into(), section: None, offset: Some(pos) }),
        };
        let tail = checked_add(reference_offset, 16)?;
        rows.push(UserValue {
            offset: start,
            variant,
            reference_offset,
            definition_guid: guid_at(data, reference_offset)?,
            integer_storage,
            number_storage,
            text_storage,
            opaque_tail_hex: bytes(data, tail, 6)?.iter().map(|b| format!("{b:02x}")).collect(),
            ordinal_storage: u32_at(data, checked_add(tail, 6)?)?,
        });
        pos = checked_add(tail, 10)?;
    }
    marker(data, pos, CLOSE)?;
    Ok((rows, checked_add(pos, 4)?.checked_sub(checked_add(at, 1)?).ok_or_else(|| fail("limit", "User values size"))?))
}
struct TaskReader<'a> {
    data: &'a [u8],
    tasks: Vec<Task>,
    seen: BTreeSet<String>,
    records: usize,
    relationships: usize,
}
impl TaskReader<'_> {
    fn task(&mut self, start: usize, parent: Option<&str>, depth: usize) -> Result<usize> {
        if depth > 64 || self.records >= RECORD_LIMIT {
            return Err(fail("limit", "Task nesting or total count exceeds budget"));
        }
        self.records += 1;
        marker(self.data, start, &[0x76, 0x25, 0xec, 0x04])?;
        let guid = guid_at(self.data, checked_add(start, 12)?)?;
        if !self.seen.insert(guid.clone()) {
            return Err(fail("invalid_identity", "Duplicate Task GUID"));
        }
        let mut at = checked_add(start, 29)?;
        marker(self.data, at, OPEN)?;
        let n = count_at(self.data, checked_add(at, 4)?)?;
        self.relationships = checked_add(self.relationships, n)?;
        if self.relationships > RECORD_LIMIT {
            return Err(fail("limit", "Total assignment/link count exceeds 10000"));
        }
        at = checked_add(at, 8)?;
        let mut assignments = Vec::new();
        for _ in 0..n {
            let record = bytes(self.data, at, 235)?;
            marker(record, 0, &[0x99, 0xd8, 0x23, 0x46])?;
            assignments.push(Assignment {
                offset: at,
                bytes: 235,
                resource_guid: guid_at(record, 131)?,
                profile_guid: guid_at(record, 12)?,
                identity_candidate: guid_at(record, 115)?,
                raw_record_hex: hex(record),
                planned_units: finite_at(record, 163)?,
                rate_storage_window: finite_at(record, 171)?,
                remaining_units_storage: optional_scalar(record, 179)?,
                remaining_rate_storage: optional_scalar(record, 187)?,
                actual_units_storage: optional_scalar(record, 195)?,
            });
            at = checked_add(at, 235)?;
        }
        marker(self.data, at, CLOSE)?;
        at = checked_add(at, 8)?;
        marker(self.data, at, OPEN)?;
        let n = count_at(self.data, checked_add(at, 4)?)?;
        self.relationships = checked_add(self.relationships, n)?;
        if self.relationships > RECORD_LIMIT {
            return Err(fail("limit", "Total assignment/link count exceeds 10000"));
        }
        at = checked_add(at, 8)?;
        let mut links = Vec::new();
        for _ in 0..n {
            let record = bytes(self.data, at, 80)?;
            marker(record, 0, &[0xd1, 0xe3, 0xe7, 0x04])?;
            links.push(Link {
                offset: at,
                predecessor_guid: guid_at(record, 28)?,
                type_storage_window: u32_at(record, 44)?,
                lag_storage_hex: bytes(record, 48, 8)?.iter().map(|v| format!("{v:02x}")).collect(),
            });
            at = checked_add(at, 80)?;
        }
        marker(self.data, at, CLOSE)?;
        at = checked_add(at, 20)?;
        let name_offset = at;
        let (name, name_end) = string_at(self.data, at)?;
        let planned_start_wallclock_scalar = u64_at(self.data, name_end)?;
        let duration_storage_scalar = u64_at(self.data, checked_add(name_end, 8)?)?;
        let flag_at = checked_add(name_end, 16)?;
        let flag = *bytes(self.data, flag_at, 1)?.first().ok_or_else(|| fail("truncated", "Missing child flag"))?;
        let child_start = checked_add(name_end, 17)?;
        let mut child_end = child_start;
        if flag == 1 {
            marker(self.data, child_end, OPEN)?;
            let n = count_at(self.data, checked_add(child_end, 4)?)?;
            child_end = checked_add(child_end, 8)?;
            for _ in 0..n {
                child_end = self.task(child_end, Some(&guid), depth + 1)?;
            }
            marker(self.data, child_end, CLOSE)?;
            child_end = checked_add(child_end, 4)?;
        } else if flag != 0 {
            return Err(fail("unsupported_layout", "Unknown child list flag"));
        }
        let mut extra = child_end.checked_sub(child_start).ok_or_else(|| fail("limit", "Child length underflow"))?;
        let (direct_costs, cost_extra) = direct_costs(self.data, child_end, RECORD_LIMIT.saturating_sub(self.relationships))?;
        self.relationships = checked_add(self.relationships, direct_costs.len())?;
        extra = checked_add(extra, cost_extra)?;
        let progress_at = checked_add(checked_add(name_end, 50)?, extra)?;
        let budget = RECORD_LIMIT.checked_sub(self.relationships).ok_or_else(|| fail("limit", "Relationship budget"))?;
        let (progress_records, progress_extra) = progress_records(self.data, progress_at, budget)?;
        self.relationships = checked_add(self.relationships, progress_records.len())?;
        let codes_at = checked_add(checked_add(checked_add(name_end, 184)?, extra)?, progress_extra)?;
        let (activity_code_refs, id_at) = code_references(self.data, codes_at, RECORD_LIMIT.saturating_sub(self.relationships))?;
        self.relationships = checked_add(self.relationships, activity_code_refs.len())?;
        let (display_id, id_end) = string_at(self.data, id_at)?;
        let (user_values, user_extra) = user_values(self.data, id_end, RECORD_LIMIT.saturating_sub(self.relationships))?;
        self.relationships = checked_add(self.relationships, user_values.len())?;
        let status_storage_value = u32_at(self.data, checked_add(checked_add(id_end, 25)?, user_extra)?)?;
        if !(1..=3).contains(&status_storage_value) {
            return Err(fail("unsupported_layout", "Unknown status storage value"));
        }
        let end = checked_add(checked_add(id_end, 121)?, user_extra)?;
        bytes(self.data, start, end.checked_sub(start).ok_or_else(|| fail("limit", "Task length"))?)?;
        // Required trailing empty list and record terminator guard the fixed tail hypothesis.
        marker(self.data, end.checked_sub(20).ok_or_else(|| fail("limit", "Tail underflow"))?, OPEN)?;
        if u32_at(self.data, end.checked_sub(16).ok_or_else(|| fail("limit", "Tail count"))?)? != 0 {
            return Err(fail("unsupported_layout", "Nonempty additional Task list is outside this reader"));
        }
        marker(self.data, end.checked_sub(12).ok_or_else(|| fail("limit", "Tail close"))?, CLOSE)?;
        marker(self.data, end.checked_sub(8).ok_or_else(|| fail("limit", "Task footer"))?, &[0x92, 0xd4, 0x70, 0xa2])?;
        let dynamic_start = checked_add(name_end, extra)?;
        let mut date_storage_windows = [0i64; 4];
        for (slot, offset) in date_storage_windows.iter_mut().zip([18, 26, 34, 42]) {
            *slot =
                i64::from_le_bytes(bytes(self.data, checked_add(dynamic_start, offset)?, 8)?.try_into().map_err(|_| fail("truncated", "Task date storage"))?);
        }
        self.tasks.push(Task {
            calendar_guid_storage: guid_at(self.data, name_offset.checked_sub(16).ok_or_else(|| fail("truncated", "Task calendar window"))?)?,
            date_storage_windows,
            raw_dynamic_window_hex: hex(bytes(self.data, dynamic_start, 184)?),
            raw_header_hex: hex(bytes(self.data, start, 29)?),
            offset: start,
            bytes: end - start,
            guid,
            parent_guid: parent.map(str::to_owned),
            name,
            display_id,
            name_offset,
            name_end,
            planned_start_wallclock_scalar,
            duration_storage_scalar,
            status_storage_value,
            progress_records,
            assignments,
            links,
            direct_costs,
            activity_code_refs,
            user_values,
        });
        Ok(end)
    }
}
fn tasks(data: &[u8]) -> Result<Vec<Task>> {
    marker(data, 24, OPEN)?;
    let n = count_at(data, 28)?;
    let mut reader = TaskReader { data, tasks: Vec::new(), seen: BTreeSet::new(), records: 0, relationships: 0 };
    let mut at = 32;
    for _ in 0..n {
        at = reader.task(at, None, 0)?;
    }
    marker(data, at, CLOSE)?;
    if checked_add(at, 4)? != data.len() {
        return Err(fail("unsupported_layout", "Unconsumed Task bytes"));
    }
    reader.tasks.sort_by_key(|t| t.offset);
    Ok(reader.tasks)
}
#[derive(Serialize)]
struct NodeBinding {
    node_guid: String,
    ordinal_storage: u32,
    source_offset: usize,
}
#[derive(Serialize)]
struct ResourceIdentity {
    offset: usize,
    guid: String,
    name: String,
    parent_guid: Option<String>,
    secondary_text_storage: String,
    opaque_text_fields: [String; 3],
    geometry_bindings: Vec<NodeBinding>,
    bytes: usize,
    child_count: usize,
    opaque_guid_list_counts: [usize; 3],
    user_values: Vec<UserValue>,
    cost_record_count: usize,
    raw_prefix_hex: String,
    raw_identity_window_hex: String,
    raw_pre_children_hex: String,
    raw_post_children_hex: String,
    kind_storage_byte: u8,
    unit_storage_byte: u8,
    cost_storage_groups: Vec<serde_json::Value>,
}
fn resources(data: &[u8]) -> Result<Vec<ResourceIdentity>> {
    resource_tree::parse(data)
}

#[derive(Serialize)]
struct Evidence {
    schema_version: u32,
    reader_version: &'static str,
    status: &'static str,
    source_sha256: String,
    admitted_build: &'static str,
    sections: BTreeMap<String, Section>,
    tasks: Vec<Task>,
    resources: Option<Vec<ResourceIdentity>>,
    coverage: Coverage,
    definition_counts: [usize; 3],
    definition_storage: BTreeMap<String, serde_json::Value>,
    raw_sections: BTreeMap<String, serde_json::Value>,
    activity_code_opaque_suffix_bytes: usize,
    limitations: Vec<&'static str>,
}
#[derive(Serialize)]
struct Coverage {
    task_record_framing_validated: bool,
    resource_record_framing_validated: bool,
    assignment_resource_refs_checked: bool,
    task_link_refs_checked: bool,
    profile_code_user_definition_records_checked: bool,
}
fn inspect_container(c: Container, source_sha256: String, tasks_only: bool) -> Result<Evidence> {
    let task_data = c.decoded.get("Tasks").ok_or_else(|| fail("missing_dependency", "Tasks section is missing"))?;
    let resource_data = c.decoded.get("Resources").ok_or_else(|| fail("missing_dependency", "Resources section is missing"))?;
    let tasks = tasks(task_data).map_err(|mut e| {
        e.section = Some("Tasks".into());
        e
    })?;
    let resources = if tasks_only {
        Vec::new()
    } else {
        resources(resource_data).map_err(|mut e| {
            e.section = Some("Resources".into());
            e
        })?
    };
    let ids = resources.iter().map(|r| r.guid.as_str()).collect::<BTreeSet<_>>();
    let task_ids = tasks.iter().map(|t| t.guid.as_str()).collect::<BTreeSet<_>>();
    let definitions = definitions::parse(&c)?;
    for task in &tasks {
        for reference in &task.activity_code_refs {
            if !definitions.codes.contains(&reference.guid) {
                return Err(fail("missing_dependency", "Code GUID absent from parsed value records"));
            }
        }
        for value in &task.user_values {
            definitions::check_user(&definitions, value, Some(1))?;
        }
        for assignment in &task.assignments {
            if !definitions.profiles.contains(&assignment.profile_guid) {
                return Err(fail("missing_dependency", "Profile GUID absent from parsed records"));
            }
        }
    }
    for resource in &resources {
        for value in &resource.user_values {
            definitions::check_user(&definitions, value, Some(2))?;
        }
    }
    for t in &tasks {
        for a in &t.assignments {
            if !tasks_only && !ids.contains(a.resource_guid.as_str()) {
                return Err(fail("missing_dependency", "Assignment references absent Resource GUID"));
            }
        }
        for link in &t.links {
            if !task_ids.contains(link.predecessor_guid.as_str()) {
                return Err(fail("missing_dependency", "Link references absent Task GUID"));
            }
        }
    }
    Ok(Evidence {
        schema_version: 4,
        reader_version: env!("CARGO_PKG_VERSION"),
        status: "partial",
        source_sha256,
        admitted_build: "6.5.5.28",
        raw_sections: c
            .decoded
            .get("Calendars")
            .map(|data| {
                BTreeMap::from([(
                    "Calendars".to_owned(),
                    serde_json::json!({"encoding":"decoded_hex","bytes":data.len(),"sha256":format!("{:x}",Sha256::digest(data)),"data_hex":hex(data)}),
                )])
            })
            .unwrap_or_default(),
        sections: c.sections,
        tasks,
        resources: if tasks_only { None } else { Some(resources) },
        definition_counts: [definitions.codes.len(), definitions.profiles.len(), definitions.users.len()],
        definition_storage: definitions.storage,
        activity_code_opaque_suffix_bytes: definitions.code_tail_bytes,
        coverage: Coverage {
            task_record_framing_validated: true,
            resource_record_framing_validated: !tasks_only,
            assignment_resource_refs_checked: !tasks_only,
            task_link_refs_checked: true,
            profile_code_user_definition_records_checked: true,
        },
        limitations: vec![
            "Experimental reader; Tasks, Resources, Use Profiles, Activity Codes and User Fields decoded only; other sections hashed as stored bytes without payload validation",
            "Direct costs admit observed singleton Fixed framing only; values remain storage fields without cost evaluation",
            "Definition records are parsed by bounded framing; referenced user variant/domain must match; unused definition parameters and Activity Codes suffix remain opaque",
            "Explicit --tasks-only scope returns resources null and leaves assignment-resource references unverified; consult typed coverage",
            "Section sha256 hashes decoded bytes only for zlib_decoded_validated; opaque_stored_unvalidated hashes wrapper plus stored payload",
            "Unused section stream validity and external dependencies are unknown; no general SP compatibility claim",
            "Output offsets are within decoded named sections, not absolute file positions",
            "Task fixed tail has opaque fields; comments, extra lists, calendars and other layouts may be unsupported",
            "Resource identity and parent hierarchy use a complete bounded framing walk; opaque GUID-list meanings, quantities, rates and cost calculations are not decoded",
            "Assignment identity_candidate is not a confirmed semantic Assignment ID",
            "planned_start_wallclock_scalar is a stored naive wall-clock value; timezone and DST semantics are not established",
            "duration_storage_scalar has not been generalized across calendars; sentinel values are retained",
            "status_storage_value 1/2/3 correlates with Planned/Started/Finished in one controlled fixture",
            "rate_storage_window is unnormalized; do not treat it as a universal rate or currency",
            "Geometry is separately validated; schedule evaluation, cost calculation and native writing are outside the project metadata scope",
            "Progress records admit only observed 88-byte empty-label/empty-nested-list layouts; scalars retain raw storage semantics",
            "All non-extracted fields and section data remain opaque; partial status is intentional",
        ],
    })
}

/// Imported scene plus unnormalized version-4 project metadata.
#[derive(Debug)]
pub struct ImportedProject {
    pub scene: scene_bundle::Scene,
    pub metadata: serde_json::Value,
}
#[derive(Serialize)]
struct Binding {
    resource_guid: String,
    node_guid: String,
    ordinal_storage: u32,
    source_offset: usize,
    source_section: &'static str,
    binding_kind: &'static str,
}
fn check(callback: &mut dyn FnMut(f32) -> bool, progress: f32) -> Result<()> {
    if callback(progress) { Ok(()) } else { Err(fail("cancelled", "Project import cancelled")) }
}
fn format_error(e: Failure) -> FormatError {
    let message = format!("project {}: {} (section {:?}, offset {:?})", e.code, e.message, e.section, e.offset);
    match e.code {
        "truncated" => FormatError::Truncated(message),
        "limit" => FormatError::TooLarge(message),
        "unsupported_layout" | "unsupported_version" | "cancelled" => FormatError::Unsupported(message),
        _ => FormatError::Malformed(message),
    }
}
fn inflate(raw: &[u8], cap: usize, callback: &mut dyn FnMut(f32) -> bool) -> Result<Vec<u8>> {
    use miniz_oxide::inflate::stream::{InflateState, inflate};
    use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut pos = 0usize;
    let mut output = Vec::new();
    loop {
        check(callback, 0.15)?;
        let mut chunk = [0u8; 65536];
        let result = inflate(&mut state, raw.get(pos..).ok_or_else(|| fail("truncated", "Compressed range"))?, &mut chunk, MZFlush::None);
        pos = checked_add(pos, result.bytes_consumed)?;
        if checked_add(output.len(), result.bytes_written)? > cap {
            return Err(fail("limit", "Decoded section budget"));
        }
        output.try_reserve(result.bytes_written).map_err(|_| fail("limit", "Decode allocation"))?;
        output.extend_from_slice(bytes(&chunk, 0, result.bytes_written)?);
        match result.status {
            Ok(MZStatus::StreamEnd) => {
                if pos != raw.len() {
                    return Err(fail("invalid_stream", "Trailing compressed bytes"));
                }
                return Ok(output);
            }
            Ok(MZStatus::Ok) | Err(MZError::Buf) if result.bytes_consumed != 0 || result.bytes_written != 0 => {}
            _ => return Err(fail("invalid_stream", "Corrupt or truncated compressed section")),
        }
    }
}
fn container(raw: &[u8], callback: &mut dyn FnMut(f32) -> bool) -> Result<Container> {
    if raw.len() > FILE_LIMIT {
        return Err(fail("limit", "Input exceeds 256 MiB"));
    }
    bytes(raw, 0, 2344)?;
    marker(raw, 0, &[0xb6, 0x17, 0x67, 0x1e, 0x7a, 0xf3, 0x46, 0x40, 0xa8, 0x6e, 0xd5, 0xdc, 0x41, 0x7a, 0x2d, 0xe2])?;
    if string_at(raw, 20)?.0 != "6.5.5.28" {
        return Err(fail("unsupported_version", "Only observed build 6.5.5.28 is admitted"));
    }
    let mut directory = Vec::new();
    let mut seen = BTreeSet::new();
    for i in 0usize..48 {
        let at = checked_add(40, i.checked_mul(48).ok_or_else(|| fail("limit", "Directory arithmetic"))?)?;
        let field = bytes(raw, at, 40)?;
        let end = field.iter().position(|v| *v == 0).ok_or_else(|| fail("invalid_directory", "Section name terminator"))?;
        let name = bytes(field, 0, end)?;
        if name.is_empty() || !name.iter().all(|v| (32..=126).contains(v)) || !field.get(end..).is_some_and(|v| v.iter().all(|b| *b == 0)) {
            return Err(fail("invalid_directory", "Section name/padding"));
        }
        let name = String::from_utf8(name.to_vec()).map_err(|_| fail("invalid_directory", "Section name encoding"))?;
        if !seen.insert(name.clone()) {
            return Err(fail("invalid_directory", "Duplicate section name"));
        }
        let offset = usize::try_from(u64_at(raw, checked_add(at, 40)?)?).map_err(|_| fail("limit", "Directory offset"))?;
        if offset < 2344 || offset >= raw.len() || directory.last().is_some_and(|(_, prior)| *prior >= offset) {
            return Err(fail("invalid_directory", "Section offset/order"));
        }
        directory.push((name, offset));
    }
    let mut sections = BTreeMap::new();
    let mut decoded = BTreeMap::new();
    let mut total = 0usize;
    for (i, (name, start)) in directory.iter().enumerate() {
        check(callback, 0.1)?;
        let end = directory.get(checked_add(i, 1)?).map(|(_, s)| *s).unwrap_or(raw.len());
        let stored = bytes(raw, *start, end.checked_sub(*start).ok_or_else(|| fail("limit", "Section length"))?)?;
        let mut prefix = vec![1, u8::try_from(name.len()).map_err(|_| fail("limit", "Section name"))?];
        prefix.extend_from_slice(name.as_bytes());
        marker(stored, 0, &prefix)?;
        let retained = matches!(name.as_str(), "Tasks" | "Resources" | "Use Profiles" | "Activity Codes" | "User Fields" | "Calendars");
        let data = if retained {
            inflate(stored.get(prefix.len()..).ok_or_else(|| fail("truncated", "Section wrapper"))?, STREAM_LIMIT, callback)?
        } else {
            Vec::new()
        };
        total = checked_add(total, data.len())?;
        if total > TOTAL_LIMIT {
            return Err(fail("limit", "Retained section total"));
        }
        sections.insert(
            name.clone(),
            Section {
                file_offset: *start,
                stored_bytes: stored.len(),
                decoded_bytes: retained.then_some(data.len()),
                sha256: format!("{:x}", Sha256::digest(if retained { &data } else { stored })),
                encoding: if retained { "zlib_decoded_validated" } else { "opaque_stored_unvalidated" },
            },
        );
        if retained {
            decoded.insert(name.clone(), data);
        }
    }
    Ok(Container { sections, decoded })
}
fn json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| fail("invalid_metadata", "Project serialization"))?;
    if bytes.len() > scene_bundle::MAX_METADATA_BYTES {
        return Err(fail("limit", "Project metadata size"));
    }
    Ok(bytes)
}
fn lines<T: Serialize>(items: &[T]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for item in items {
        let line = json(item)?;
        if line.len() > scene_bundle::MAX_RECORD_BYTES || checked_add(checked_add(out.len(), line.len())?, 1)? > scene_bundle::MAX_METADATA_BYTES {
            return Err(fail("limit", "Project records size"));
        }
        out.try_reserve(checked_add(line.len(), 1)?).map_err(|_| fail("limit", "Metadata allocation"))?;
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    Ok(out)
}
/// Parse one admitted project layout; returning false cooperatively cancels.
/// No source programs, files, external assets or processes are accessed.
pub fn parse(raw: &[u8], mut keep_going: impl FnMut(f32) -> bool) -> std::result::Result<ImportedProject, FormatError> {
    let mut progress = 0.0f32;
    parse_inner(raw, &mut |p| {
        progress = progress.max(p);
        keep_going(progress)
    })
    .map_err(format_error)
}
fn parse_inner(raw: &[u8], callback: &mut dyn FnMut(f32) -> bool) -> Result<ImportedProject> {
    check(callback, 0.)?;
    let c = container(raw, callback)?;
    let mut source_hash = Sha256::new();
    for chunk in raw.chunks(65536) {
        check(callback, 0.25)?;
        source_hash.update(chunk);
    }
    let source_sha = format!("{:x}", source_hash.finalize());
    let index = definitions::parse(&c)?;
    let mut geometry = scene_geometry::Geometry::new(callback)?;
    let mut total = 0usize;
    for name in ["Entities", "IncludeLibrary"] {
        let section = c.sections.get(name).ok_or_else(|| fail("missing_dependency", "Geometry section missing"))?;
        let prefix = checked_add(name.len(), 2)?;
        let data = bytes(
            raw,
            checked_add(section.file_offset, prefix)?,
            section.stored_bytes.checked_sub(prefix).ok_or_else(|| fail("truncated", "Geometry wrapper"))?,
        )?;
        let decoded = inflate(data, 192 * 1024 * 1024, geometry.keep_callback())?;
        total = checked_add(total, decoded.len())?;
        if total > 320 * 1024 * 1024 {
            return Err(fail("limit", "Geometry total"));
        }
        geometry.section(&decoded, name, &index)?;
    }
    let evidence = inspect_container(c, source_sha.clone(), false)?;
    let mut bindings = Vec::new();
    if let Some(resources) = &evidence.resources {
        let byid = geometry.nodes.iter().map(|n| (n.guid.as_str(), n)).collect::<BTreeMap<_, _>>();
        for resource in resources {
            for b in &resource.geometry_bindings {
                let target = byid.get(b.node_guid.as_str()).ok_or_else(|| fail("missing_dependency", "Resource geometry missing"))?;
                if target.section != "Entities" {
                    return Err(fail("unsupported_layout", "Direct binding outside Entities"));
                }
                bindings.push(Binding {
                    resource_guid: resource.guid.clone(),
                    node_guid: b.node_guid.clone(),
                    ordinal_storage: b.ordinal_storage,
                    source_offset: b.source_offset,
                    source_section: "Resources",
                    binding_kind: "direct_subtree",
                });
            }
        }
    }
    // Retain shared mesh definitions while the graph expands only instance metadata.
    let binary = std::mem::take(&mut geometry.binary);
    let instances = scene_graph::build(&geometry.nodes, &geometry.meshes, &geometry.roots, &bindings, &source_sha, geometry.keep_going)?;
    let entities = geometry.nodes.iter().filter(|n| n.section == "Entities").count();
    let manifest = serde_json::json!({"schema_version":1,"status":"experimental_scene_partial","source_sha256":source_sha,"entities":entities,"library_nodes":geometry.nodes.len()-entities,"meshes":geometry.meshes.len(),"vertices":geometry.meshes.iter().map(|m|m.vertex_count).sum::<usize>(),"triangles":geometry.meshes.iter().map(|m|m.triangle_count).sum::<usize>(),"instances":instances.len(),"direct_resource_bindings":bindings.len(),"binary_bytes":binary.len(),"coordinate_unit":"unverified_native_world_unit","world_axis_convention":"unverified_native_world_axes","artifact_layout":{"nodes":"nodes.jsonl","meshes":"meshes.jsonl","instances":"instances.jsonl","resource_bindings":"resource-bindings.jsonl","project":"project.json","binary":"geometry.bin","positions":"little-endian f64 xyz, 24 bytes per vertex","triangle_indices":"little-endian u32 abc, 12 bytes per triangle"}});
    let nodes = lines(&geometry.nodes)?;
    let meshes = lines(&geometry.meshes)?;
    let instances = lines(&instances)?;
    let bindings = lines(&bindings)?;
    drop(geometry);
    check(callback, 0.75)?;
    let project = json(&evidence)?;
    let manifest = json(&manifest)?;
    let metadata = serde_json::from_slice(&project).map_err(|_| fail("invalid_metadata", "Metadata representation"))?;
    let scene = scene_bundle::parse(
        scene_bundle::Inputs {
            manifest: &manifest,
            nodes: &nodes,
            meshes: &meshes,
            instances: &instances,
            bindings: &bindings,
            project: &project,
            binary: &binary,
        },
        |p| callback(0.75 + p * 0.25),
    )
    .map_err(|e| match e {
        FormatError::TooLarge(s) => fail("limit", s),
        FormatError::Truncated(s) => fail("truncated", s),
        FormatError::Unsupported(s) => fail("unsupported_layout", s),
        FormatError::Malformed(s) => fail("invalid_scene", s),
    })?;
    Ok(ImportedProject { scene, metadata })
}

fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

fn optional_scalar(data: &[u8], at: usize) -> Result<Option<f64>> {
    let v = f64::from_bits(u64_at(data, at)?);
    Ok(v.is_finite().then_some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(value: f32, vertex_index: u32) -> Vec<u8> {
        let mut b = vec![0; 24];
        b.extend(OPEN);
        b.extend(1u32.to_le_bytes());
        b.extend([0x37, 0x2a, 0x1a, 0x54]);
        b.extend([0; 8]);
        b.extend([1; 16]);
        b.extend([255, 254, 255, 0]);
        b.extend([0, 0, 0, 0, 1]);
        b.extend([0xae, 0xec, 0x55, 0xca]);
        for _ in 0..13 {
            b.extend([0, 1]);
        }
        b.extend([1, 1]);
        b.extend(1u32.to_le_bytes());
        b.extend([0xb6, 0x69, 0x4a, 0x30]);
        b.extend(3u32.to_le_bytes());
        for v in [value, 0., 0., 1., 0., 0., 0., 1., 0.] {
            b.extend(v.to_le_bytes());
        }
        b.extend(4u32.to_le_bytes());
        for v in [3, 0, 1, vertex_index] {
            b.extend(v.to_le_bytes());
        }
        b.extend([1, 0xc0, 0x5a, 0x01, 0xf3]);
        b.extend([0; 9]);
        b.extend([0, 0]);
        b.extend([0; 8]);
        b.extend([0; 4]);
        b.extend([0; 12]);
        b.extend([0x1c, 0xb3, 0x2c, 0x53]);
        b.extend([0, 1, 0xd2, 0xa0, 0x97, 0x39]);
        for _ in 0..19 {
            b.extend([0, 1]);
        }
        b.extend([0xca, 0x82, 0x13, 0x15]);
        b.extend([0, 0, 0, 1, 1, 0]);
        b.extend([0; 5]);
        b.extend([0xd0, 0xbc, 0xa1, 0x06]);
        b.extend([0; 4]);
        b.extend([0x2c, 0x57, 0xd9, 0xda]);
        b.extend([0, 0x73, 0x71, 0x5f, 0xd7]);
        b.extend([0; 4]);
        b.extend(CLOSE);
        b
    }

    fn empty_section() -> Vec<u8> {
        let mut b = vec![0; 24];
        b.extend(OPEN);
        b.extend(0u32.to_le_bytes());
        b.extend(CLOSE);
        b
    }
    fn project(geometry: Vec<u8>) -> Vec<u8> {
        let names = ["Tasks", "Resources", "Use Profiles", "Activity Codes", "User Fields", "Entities", "IncludeLibrary", "Calendars"];
        let mut sections = Vec::new();
        for i in 0..48 {
            let name = names.get(i).map(|s| s.to_string()).unwrap_or_else(|| format!("Opaque{i}"));
            let payload = if name == "Entities" { geometry.clone() } else { empty_section() };
            let mut stored = vec![1, name.len() as u8];
            stored.extend(name.as_bytes());
            stored.extend(miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6));
            sections.push((name, stored));
        }
        let mut b = vec![0; 2344];
        let magic = [0xb6, 0x17, 0x67, 0x1e, 0x7a, 0xf3, 0x46, 0x40, 0xa8, 0x6e, 0xd5, 0xdc, 0x41, 0x7a, 0x2d, 0xe2];
        if let Some(v) = b.get_mut(..16) {
            v.copy_from_slice(&magic);
        }
        let mut version = vec![255, 254, 255, 8];
        for c in "6.5.5.28".encode_utf16() {
            version.extend(c.to_le_bytes());
        }
        if let Some(v) = b.get_mut(20..40) {
            v.copy_from_slice(&version);
        }
        for (i, (name, payload)) in sections.iter().enumerate() {
            let at = 40 + i * 48;
            let offset = b.len() as u64;
            if let Some(v) = b.get_mut(at..at + name.len()) {
                v.copy_from_slice(name.as_bytes());
            }
            if let Some(v) = b.get_mut(at + 40..at + 48) {
                v.copy_from_slice(&offset.to_le_bytes());
            }
            b.extend(payload);
        }
        b
    }
    #[test]
    fn admitted_project_opens_and_retains_metadata() {
        let result = parse(&project(fixture(0., 2)), |_| true);
        assert!(result.is_ok(), "{result:?}");
        if let Ok(p) = result {
            assert_eq!(p.scene.meshes.len(), 1);
            assert_eq!(p.scene.triangles, 1);
            assert_eq!(p.metadata.get("schema_version").and_then(|v| v.as_u64()), Some(4));
        }
    }
    #[test]
    fn malformed_geometry_and_truncated_inputs_reject() {
        for data in [fixture(f32::NAN, 2), fixture(0., 3)] {
            assert!(parse(&project(data), |_| true).is_err());
        }
        let mut truncated_geometry = fixture(0., 2);
        truncated_geometry.pop();
        assert!(parse(&project(truncated_geometry), |_| true).is_err());
        let raw = project(fixture(0., 2));
        for n in [0, 16, 40, 2343] {
            assert!(parse(raw.get(..n).unwrap_or(&[]), |_| true).is_err());
        }
    }
    #[test]
    fn cancellation_and_exact_compressed_consumption() {
        assert!(parse(&[], |_| false).is_err());
        let raw = project(fixture(0., 2));
        let mut calls = 0;
        assert!(
            parse(&raw, |_| {
                calls += 1;
                calls < 12
            })
            .is_err()
        );
        let mut z = miniz_oxide::deflate::compress_to_vec_zlib(b"ok", 6);
        z.push(0);
        assert!(inflate(&z, 100, &mut |_| true).is_err());
        assert!(inflate(&miniz_oxide::deflate::compress_to_vec_zlib(&[0; 1000], 6), 99, &mut |_| true).is_err());
    }
    #[test]
    fn unsupported_build_and_duplicate_directory_reject() {
        let mut raw = project(fixture(0., 2));
        if let Some(v) = raw.get_mut(24) {
            *v = b'7';
        }
        assert!(matches!(parse(&raw, |_| true), Err(FormatError::Unsupported(_))));
        let mut raw = project(fixture(0., 2));
        let first = raw.get(40..80).unwrap_or(&[]).to_vec();
        if let Some(v) = raw.get_mut(88..128) {
            v.copy_from_slice(&first);
        }
        assert!(parse(&raw, |_| true).is_err());
    }
}
