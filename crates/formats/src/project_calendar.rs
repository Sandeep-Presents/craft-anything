//! Bounded calendars for one observed project serialization layout.
//!
//! Scalars are naive wall-clock seconds, not verified UTC instants. Sunday-first
//! weekday ordering and the 86400-second interval base are inferred from paired
//! native controls. Parent exception inheritance and nil weekday inheritance
//! remain unsupported rather than guessed.
use crate::FormatError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, FormatError>;
const OPEN: [u8; 4] = [0x7a, 0x54, 0xaf, 0x39];
const CLOSE: [u8; 4] = [0x16, 0xe4, 0x99, 0x6f];
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CALENDARS: usize = 10_000;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_DEPTH: usize = 64;
pub const MAX_SCAN_DAYS: u32 = 36_600;
pub const MAX_SEGMENTS: usize = 65_536;
pub const MAX_WORK_SECONDS: u64 = 315_576_000;
pub const UNSPECIFIED_WALLCLOCK: i64 = 32_535_244_799;
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConventionEvidence {
    InferredFromNativeControls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conventions {
    /// Index zero is Sunday; index one is Monday.
    pub sunday_first_weekday_order: ConventionEvidence,
    /// Observed 09:00 is stored as 118800, not 32400.
    pub interval_base_seconds: i64,
    pub interval_base_evidence: ConventionEvidence,
    /// No UTC/DST interpretation has been established.
    pub naive_wallclock_only: bool,
    pub ancestor_exception_inheritance_verified: bool,
}

impl Default for Conventions {
    fn default() -> Self {
        Self {
            sunday_first_weekday_order: ConventionEvidence::InferredFromNativeControls,
            interval_base_seconds: DAY,
            interval_base_evidence: ConventionEvidence::InferredFromNativeControls,
            naive_wallclock_only: true,
            ancestor_exception_inheritance_verified: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingInterval {
    pub raw_start_wallclock: i64,
    pub raw_end_wallclock: i64,
    pub start_second: u32,
    pub end_second: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayType {
    pub offset: usize,
    pub guid: String,
    pub name: String,
    pub color_storage: [f32; 3],
    /// Observed 1=working, 2=non-working. Other values are not evaluated.
    pub kind_storage: u32,
    pub intervals: Vec<WorkingInterval>,
    pub ordinal_storage: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateException {
    pub offset: usize,
    pub date_wallclock: i64,
    pub day_type_guid: String,
    pub ordinal_storage: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    pub offset: usize,
    pub guid: String,
    pub name: String,
    pub parent_guid: Option<String>,
    pub weekday_day_type_guids: [Option<String>; 7],
    pub exceptions: Vec<DateException>,
    pub day_types: Vec<DayType>,
    pub ordinal_storage: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calendars {
    pub schema_version: u32,
    pub source_bytes: usize,
    pub raw_header_hex: String,
    pub root_calendar_guids: Vec<String>,
    pub default_calendar_guid: Option<String>,
    pub records: Vec<Calendar>,
    pub conventions: Conventions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingBudget {
    pub max_scanned_days: u32,
    pub max_segments: usize,
}

impl Default for WorkingBudget {
    fn default() -> Self {
        Self { max_scanned_days: MAX_SCAN_DAYS, max_segments: MAX_SEGMENTS }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSegment {
    pub start_wallclock: i64,
    pub end_wallclock: i64,
    pub working_seconds: u64,
    pub day_type_guid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkAllocation {
    pub calendar_guid: String,
    pub requested_start_wallclock: i64,
    pub first_work_start_wallclock: Option<i64>,
    pub finish_wallclock: i64,
    pub requested_working_seconds: u64,
    pub segments: Vec<WorkSegment>,
    pub inferred_storage_conventions_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum CalendarError {
    #[error("calendar data failed validation: {reason}")]
    InvalidData { reason: String },
    #[error("calendar reference is missing")]
    MissingCalendar { guid: Option<String> },
    #[error("calendar calculation was cancelled")]
    Cancelled,
    #[error("calendar calculation exceeds {budget}")]
    Limit { budget: String },
    #[error("calendar date arithmetic overflow")]
    DateOverflow,
    #[error("calendar interpretation is unsupported: {reason}")]
    Unsupported { reason: String },
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

fn malformed(message: &str) -> FormatError {
    FormatError::Malformed(message.into())
}

fn too_large(message: &str) -> FormatError {
    FormatError::TooLarge(message.into())
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| too_large("calendar offset arithmetic"))?;
        let result = self.data.get(self.at..end).ok_or_else(|| FormatError::Truncated("calendar field".into()))?;
        self.at = end;
        Ok(result)
    }
    fn marker(&mut self, marker: &[u8]) -> Result<()> {
        if self.take(marker.len())? != marker {
            return Err(FormatError::Unsupported("calendar record marker/layout".into()));
        }
        Ok(())
    }
    fn u32(&mut self) -> Result<u32> {
        let value = self.take(4)?.try_into().map_err(|_| FormatError::Truncated("calendar integer".into()))?;
        Ok(u32::from_le_bytes(value))
    }
    fn i64(&mut self) -> Result<i64> {
        let value = self.take(8)?.try_into().map_err(|_| FormatError::Truncated("calendar scalar".into()))?;
        Ok(i64::from_le_bytes(value))
    }
    fn count(&mut self, cap: usize) -> Result<usize> {
        let n = usize::try_from(self.u32()?).map_err(|_| too_large("calendar count"))?;
        if n > cap {
            return Err(too_large("calendar count budget"));
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        self.marker(&[255, 254, 255])?;
        let n = usize::from(*self.take(1)?.first().ok_or_else(|| FormatError::Truncated("calendar string count".into()))?);
        let n = if n == 255 {
            usize::from(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| FormatError::Truncated("calendar long-string count".into()))?))
        } else {
            n
        };
        if n > 16_384 {
            return Err(too_large("calendar string budget"));
        }
        let bytes = n.checked_mul(2).ok_or_else(|| too_large("calendar string arithmetic"))?;
        let units = self.take(bytes)?.as_chunks::<2>().0.iter().map(|x| u16::from_le_bytes(*x)).collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|_| malformed("calendar UTF-16"))
    }
    fn guid(&mut self) -> Result<Option<String>> {
        let raw = self.take(16)?;
        if raw.iter().all(|b| *b == 0) {
            return Ok(None);
        }
        let a = u32::from_le_bytes(raw.get(..4).ok_or_else(|| malformed("GUID"))?.try_into().map_err(|_| malformed("GUID"))?);
        let b = u16::from_le_bytes(raw.get(4..6).ok_or_else(|| malformed("GUID"))?.try_into().map_err(|_| malformed("GUID"))?);
        let c = u16::from_le_bytes(raw.get(6..8).ok_or_else(|| malformed("GUID"))?.try_into().map_err(|_| malformed("GUID"))?);
        let tail = raw.get(8..).ok_or_else(|| malformed("GUID"))?.iter().map(|b| format!("{b:02x}")).collect::<String>();
        Ok(Some(format!("{a:08x}-{b:04x}-{c:04x}-{}-{}", tail.get(..4).ok_or_else(|| malformed("GUID"))?, tail.get(4..).ok_or_else(|| malformed("GUID"))?)))
    }
    fn required_guid(&mut self) -> Result<String> {
        self.guid()?.ok_or_else(|| malformed("nil calendar/day-type GUID"))
    }
}

struct Parser<'a> {
    cursor: Cursor<'a>,
    records: Vec<Calendar>,
    seen: BTreeSet<String>,
    items: usize,
}

impl Parser<'_> {
    fn items(&mut self, n: usize) -> Result<()> {
        self.items = self.items.checked_add(n).ok_or_else(|| too_large("calendar item arithmetic"))?;
        if self.items > MAX_ITEMS {
            return Err(too_large("calendar aggregate items"));
        }
        Ok(())
    }
    fn calendar(&mut self, parent: Option<&str>, depth: usize) -> Result<String> {
        if depth > MAX_DEPTH || self.seen.len() >= MAX_CALENDARS {
            return Err(too_large("calendar depth/record count"));
        }
        let offset = self.cursor.at;
        self.cursor.marker(&[0x1d, 0x26, 0xec, 0x7f])?;
        self.cursor.marker(&[0; 8])?;
        let guid = self.cursor.required_guid()?;
        if !self.seen.insert(guid.clone()) {
            return Err(malformed("duplicate calendar GUID"));
        }
        let name = self.cursor.string()?;
        self.cursor.marker(&OPEN)?;
        let children = self.cursor.count(MAX_CALENDARS)?;
        for _ in 0..children {
            self.calendar(Some(&guid), depth + 1)?;
        }
        self.cursor.marker(&CLOSE)?;
        self.cursor.marker(&[0xa9, 0x5c, 0xcd, 0x47])?;
        let mut weekday_day_type_guids = std::array::from_fn(|_| None);
        for value in &mut weekday_day_type_guids {
            *value = self.cursor.guid()?;
        }
        self.cursor.marker(&[0x25, 0xf2, 0x78, 0x41])?;
        self.cursor.marker(&OPEN)?;
        let count = self.cursor.count(MAX_ITEMS)?;
        self.items(count)?;
        let mut exceptions = Vec::new();
        for _ in 0..count {
            let offset = self.cursor.at;
            self.cursor.marker(&[0x6c, 0x3d, 0x1a, 0x0d])?;
            let date_wallclock = self.cursor.i64()?;
            if date_wallclock.rem_euclid(DAY) != 0 {
                return Err(FormatError::Unsupported("non-midnight calendar date exception".into()));
            }
            let day_type_guid = self.cursor.required_guid()?;
            self.cursor.marker(&[0x48, 0xeb, 0x61, 0xb0])?;
            let ordinal_storage = self.cursor.u32()?;
            exceptions.push(DateException { offset, date_wallclock, day_type_guid, ordinal_storage });
        }
        self.cursor.marker(&CLOSE)?;
        self.cursor.marker(&OPEN)?;
        let count = self.cursor.count(MAX_ITEMS)?;
        self.items(count)?;
        let mut day_types = Vec::new();
        for _ in 0..count {
            let offset = self.cursor.at;
            self.cursor.marker(&[0x1d, 0xc2, 0xf4, 0xc4])?;
            let guid = self.cursor.required_guid()?;
            let name = self.cursor.string()?;
            let mut color_storage = [0.; 3];
            for channel in &mut color_storage {
                *channel = f32::from_bits(self.cursor.u32()?);
                if !channel.is_finite() {
                    return Err(malformed("nonfinite calendar day color"));
                }
            }
            let kind_storage = self.cursor.u32()?;
            let count = self.cursor.count(MAX_ITEMS)?;
            self.items(count)?;
            let mut intervals = Vec::new();
            let mut previous_end = 0;
            for _ in 0..count {
                let raw_start_wallclock = self.cursor.i64()?;
                let raw_end_wallclock = self.cursor.i64()?;
                let start = raw_start_wallclock.checked_sub(DAY).ok_or_else(|| malformed("calendar interval underflow"))?;
                let end = raw_end_wallclock.checked_sub(DAY).ok_or_else(|| malformed("calendar interval underflow"))?;
                if start < 0 || end > DAY {
                    return Err(FormatError::Unsupported("unobserved calendar interval base/overnight interval".into()));
                }
                if start >= end || start < previous_end {
                    return Err(malformed("reversed/overlapping calendar intervals"));
                }
                previous_end = end;
                intervals.push(WorkingInterval {
                    raw_start_wallclock,
                    raw_end_wallclock,
                    start_second: u32::try_from(start).map_err(|_| malformed("calendar interval"))?,
                    end_second: u32::try_from(end).map_err(|_| malformed("calendar interval"))?,
                });
            }
            self.cursor.marker(&[0x19, 0x06, 0x8d, 0x63])?;
            let ordinal_storage = self.cursor.u32()?;
            day_types.push(DayType { offset, guid, name, color_storage, kind_storage, intervals, ordinal_storage });
        }
        self.cursor.marker(&CLOSE)?;
        self.cursor.marker(&[0x19, 0xea, 0x70, 0x7d])?;
        let ordinal_storage = self.cursor.u32()?;
        self.records.push(Calendar {
            offset,
            guid: guid.clone(),
            name,
            parent_guid: parent.map(str::to_owned),
            weekday_day_type_guids,
            exceptions,
            day_types,
            ordinal_storage,
        });
        Ok(guid)
    }
}

struct Index<'a> {
    calendars: BTreeMap<&'a str, &'a Calendar>,
    day_types: BTreeMap<&'a str, (&'a str, &'a DayType)>,
    exceptions: BTreeMap<&'a str, BTreeMap<i64, &'a str>>,
}
impl Calendars {
    fn index(&self) -> Result<Index<'_>> {
        if self.schema_version != 1 {
            return Err(FormatError::Unsupported("calendar schema version".into()));
        }
        if self.records.len() > MAX_CALENDARS {
            return Err(too_large("calendar record budget"));
        }
        if self.conventions != Conventions::default() {
            return Err(FormatError::Unsupported("unverified calendar interpretation override".into()));
        }
        let mut calendars = BTreeMap::new();
        let mut day_types = BTreeMap::new();
        let mut exception_index = BTreeMap::new();
        let mut items = 0usize;
        for c in &self.records {
            if c.guid.is_empty() || c.guid == "00000000-0000-0000-0000-000000000000" || calendars.insert(c.guid.as_str(), c).is_some() {
                return Err(malformed("nil/duplicate calendar GUID"));
            }
            items = items.checked_add(c.exceptions.len()).and_then(|n| n.checked_add(c.day_types.len())).ok_or_else(|| too_large("calendar items"))?;
            if items > MAX_ITEMS {
                return Err(too_large("calendar aggregate items"));
            }
            let mut dates = BTreeMap::new();
            for e in &c.exceptions {
                if e.date_wallclock.rem_euclid(DAY) != 0 {
                    return Err(malformed("calendar exception midnight"));
                }
                if dates.insert(e.date_wallclock, e.day_type_guid.as_str()).is_some_and(|old| old != e.day_type_guid) {
                    return Err(malformed("conflicting date exceptions"));
                }
            }
            exception_index.insert(c.guid.as_str(), dates);
            for d in &c.day_types {
                items = items.checked_add(d.intervals.len()).ok_or_else(|| too_large("calendar intervals"))?;
                if items > MAX_ITEMS {
                    return Err(too_large("calendar aggregate items"));
                }
                if d.guid.is_empty() || d.guid == "00000000-0000-0000-0000-000000000000" || day_types.insert(d.guid.as_str(), (c.guid.as_str(), d)).is_some() {
                    return Err(malformed("nil/duplicate day-type GUID"));
                }
                if d.color_storage.iter().any(|v| !v.is_finite()) {
                    return Err(malformed("calendar color"));
                }
                let mut previous = 0;
                for interval in &d.intervals {
                    if interval.start_second >= interval.end_second
                        || interval.end_second > DAY as u32
                        || interval.start_second < previous
                        || interval.raw_start_wallclock != DAY + i64::from(interval.start_second)
                        || interval.raw_end_wallclock != DAY + i64::from(interval.end_second)
                    {
                        return Err(malformed("calendar interval normalization/order"));
                    }
                    previous = interval.end_second;
                }
                if d.kind_storage == 2 && !d.intervals.is_empty() {
                    return Err(malformed("non-working day contains working intervals"));
                }
            }
        }
        if items > MAX_ITEMS {
            return Err(too_large("calendar aggregate items"));
        }
        let roots = self.root_calendar_guids.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let actual = self.records.iter().filter(|c| c.parent_guid.is_none()).map(|c| c.guid.as_str()).collect::<BTreeSet<_>>();
        if roots.len() != self.root_calendar_guids.len() || roots != actual {
            return Err(malformed("calendar root references"));
        }
        if self.default_calendar_guid.as_ref().is_some_and(|g| !calendars.contains_key(g.as_str())) {
            return Err(malformed("default calendar is missing"));
        }
        for c in &self.records {
            let mut chain = BTreeSet::new();
            let mut current = Some(c.guid.as_str());
            while let Some(id) = current {
                if chain.len() > MAX_DEPTH || !chain.insert(id) {
                    return Err(malformed("calendar parent cycle/depth"));
                }
                let node = calendars.get(id).ok_or_else(|| malformed("calendar parent is missing"))?;
                current = node.parent_guid.as_deref();
            }
            for id in c.weekday_day_type_guids.iter().filter_map(Option::as_deref).chain(c.exceptions.iter().map(|e| e.day_type_guid.as_str())) {
                let (owner, _) = day_types.get(id).ok_or_else(|| malformed("calendar day-type reference is missing"))?;
                if !chain.contains(owner) {
                    return Err(FormatError::Unsupported("day-type reference outside calendar ancestry".into()));
                }
            }
        }
        Ok(Index { calendars, day_types, exceptions: exception_index })
    }

    pub fn working_segments(
        &self,
        calendar_guid: Option<&str>,
        start_wallclock: i64,
        duration_seconds: u64,
    ) -> std::result::Result<WorkAllocation, CalendarError> {
        self.working_segments_with_budget(calendar_guid, start_wallclock, duration_seconds, WorkingBudget::default(), || false)
    }

    pub fn working_segments_with_budget<F: FnMut() -> bool>(
        &self,
        calendar_guid: Option<&str>,
        start_wallclock: i64,
        duration_seconds: u64,
        budget: WorkingBudget,
        mut cancelled: F,
    ) -> std::result::Result<WorkAllocation, CalendarError> {
        if cancelled() {
            return Err(CalendarError::Cancelled);
        }
        if duration_seconds > MAX_WORK_SECONDS || budget.max_scanned_days > MAX_SCAN_DAYS || budget.max_segments > MAX_SEGMENTS {
            return Err(CalendarError::Limit { budget: "working-time request/budget".into() });
        }
        if start_wallclock == UNSPECIFIED_WALLCLOCK {
            return Err(CalendarError::Unsupported { reason: "unspecified wall-clock scalar".into() });
        }
        let index = self.index().map_err(|e| CalendarError::InvalidData { reason: e.to_string() })?;
        let id = calendar_guid.or(self.default_calendar_guid.as_deref()).ok_or(CalendarError::MissingCalendar { guid: None })?;
        let calendar = index.calendars.get(id).copied().ok_or_else(|| CalendarError::MissingCalendar { guid: Some(id.into()) })?;
        let mut result = WorkAllocation {
            calendar_guid: id.into(),
            requested_start_wallclock: start_wallclock,
            first_work_start_wallclock: None,
            finish_wallclock: start_wallclock,
            requested_working_seconds: duration_seconds,
            segments: Vec::new(),
            inferred_storage_conventions_used: true,
        };
        if duration_seconds == 0 {
            return Ok(result);
        }
        let mut day = start_wallclock.div_euclid(DAY);
        let mut remaining = duration_seconds;
        for _ in 0..budget.max_scanned_days {
            if cancelled() {
                return Err(CalendarError::Cancelled);
            }
            let midnight = day.checked_mul(DAY).ok_or(CalendarError::DateOverflow)?;
            let own_exception = index.exceptions.get(id).and_then(|dates| dates.get(&midnight)).copied();
            let day_guid = if let Some(guid) = own_exception {
                guid
            } else {
                let mut parent = calendar.parent_guid.as_deref();
                while let Some(id) = parent {
                    if cancelled() {
                        return Err(CalendarError::Cancelled);
                    }
                    let p = index.calendars.get(id).copied().ok_or_else(|| CalendarError::MissingCalendar { guid: Some(id.into()) })?;
                    if index.exceptions.get(p.guid.as_str()).is_some_and(|dates| dates.contains_key(&midnight)) {
                        return Err(CalendarError::Unsupported { reason: "ancestor exception inheritance has not been validated".into() });
                    }
                    parent = p.parent_guid.as_deref();
                }
                let weekday = usize::try_from((day.rem_euclid(7) + 4).rem_euclid(7)).map_err(|_| CalendarError::DateOverflow)?;
                calendar
                    .weekday_day_type_guids
                    .get(weekday)
                    .and_then(Option::as_deref)
                    .ok_or_else(|| CalendarError::Unsupported { reason: "nil weekday/inherited weekday has not been validated".into() })?
            };
            let (_, kind) = index.day_types.get(day_guid).copied().ok_or_else(|| CalendarError::InvalidData { reason: "day type missing".into() })?;
            match kind.kind_storage {
                2 => {}
                1 => {
                    if kind.intervals.is_empty() {
                        return Err(CalendarError::Unsupported { reason: "working day without intervals".into() });
                    }
                    for interval in &kind.intervals {
                        if cancelled() {
                            return Err(CalendarError::Cancelled);
                        }
                        let a = midnight.checked_add(i64::from(interval.start_second)).ok_or(CalendarError::DateOverflow)?.max(start_wallclock);
                        let b = midnight.checked_add(i64::from(interval.end_second)).ok_or(CalendarError::DateOverflow)?;
                        if a >= b {
                            continue;
                        }
                        let available = u64::try_from(b.checked_sub(a).ok_or(CalendarError::DateOverflow)?).map_err(|_| CalendarError::DateOverflow)?;
                        let used = available.min(remaining);
                        let end = a.checked_add(i64::try_from(used).map_err(|_| CalendarError::DateOverflow)?).ok_or(CalendarError::DateOverflow)?;
                        if result.segments.len() >= budget.max_segments {
                            return Err(CalendarError::Limit { budget: "working segments".into() });
                        }
                        result.first_work_start_wallclock.get_or_insert(a);
                        result.finish_wallclock = end;
                        result.segments.push(WorkSegment { start_wallclock: a, end_wallclock: end, working_seconds: used, day_type_guid: day_guid.into() });
                        remaining -= used;
                        if remaining == 0 {
                            return Ok(result);
                        }
                    }
                }
                _ => return Err(CalendarError::Unsupported { reason: "unobserved working-day type".into() }),
            }
            day = day.checked_add(1).ok_or(CalendarError::DateOverflow)?;
        }
        Err(CalendarError::Limit { budget: "scanned calendar days".into() })
    }
}

pub fn parse(data: &[u8]) -> Result<Calendars> {
    if data.len() > MAX_BYTES {
        return Err(too_large("calendar section bytes"));
    }
    let mut parser = Parser { cursor: Cursor { data, at: 0 }, records: Vec::new(), seen: BTreeSet::new(), items: 0 };
    let raw_header_hex = parser.cursor.take(24)?.iter().map(|b| format!("{b:02x}")).collect();
    parser.cursor.marker(&OPEN)?;
    let count = parser.cursor.count(MAX_CALENDARS)?;
    let mut root_calendar_guids = Vec::new();
    for _ in 0..count {
        root_calendar_guids.push(parser.calendar(None, 0)?);
    }
    parser.cursor.marker(&CLOSE)?;
    let default_calendar_guid = parser.cursor.guid()?;
    if parser.cursor.at != data.len() {
        return Err(FormatError::Unsupported("unconsumed calendar section bytes".into()));
    }
    let result = Calendars {
        schema_version: 1,
        source_bytes: data.len(),
        raw_header_hex,
        root_calendar_guids,
        default_calendar_guid,
        records: parser.records,
        conventions: Conventions::default(),
    };
    result.index()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn guid(id: u8) -> String {
        format!("{id:08x}-0000-0000-0000-000000000000")
    }
    fn interval(start: u32, end: u32) -> WorkingInterval {
        WorkingInterval { raw_start_wallclock: DAY + i64::from(start), raw_end_wallclock: DAY + i64::from(end), start_second: start, end_second: end }
    }
    fn data() -> Calendars {
        let working = DayType {
            offset: 0,
            guid: guid(2),
            name: "Working".into(),
            color_storage: [0.; 3],
            kind_storage: 1,
            intervals: vec![interval(9 * 3600, 17 * 3600)],
            ordinal_storage: 0,
        };
        let nonworking = DayType {
            offset: 0,
            guid: guid(3),
            name: "Non-working".into(),
            color_storage: [0.; 3],
            kind_storage: 2,
            intervals: Vec::new(),
            ordinal_storage: 1,
        };
        let calendar = Calendar {
            offset: 32,
            guid: guid(1),
            name: "Calendar".into(),
            parent_guid: None,
            weekday_day_type_guids: std::array::from_fn(|i| Some(guid(if i == 0 || i == 6 { 3 } else { 2 }))),
            exceptions: Vec::new(),
            day_types: vec![working, nonworking],
            ordinal_storage: 0,
        };
        Calendars {
            schema_version: 1,
            source_bytes: 0,
            raw_header_hex: String::new(),
            root_calendar_guids: vec![guid(1)],
            default_calendar_guid: Some(guid(1)),
            records: vec![calendar],
            conventions: Conventions::default(),
        }
    }
    fn text(value: &str) -> Vec<u8> {
        let mut b = vec![255, 254, 255, value.len() as u8];
        for c in value.encode_utf16() {
            b.extend(c.to_le_bytes());
        }
        b
    }
    fn raw_guid(id: u8) -> [u8; 16] {
        let mut b = [0; 16];
        b[0] = id;
        b
    }
    fn encoded() -> Vec<u8> {
        let mut b = vec![0; 24];
        b.extend(OPEN);
        b.extend(1u32.to_le_bytes());
        b.extend([0x1d, 0x26, 0xec, 0x7f]);
        b.extend([0; 8]);
        b.extend(raw_guid(1));
        b.extend(text("Calendar"));
        b.extend(OPEN);
        b.extend(0u32.to_le_bytes());
        b.extend(CLOSE);
        b.extend([0xa9, 0x5c, 0xcd, 0x47]);
        for day in [3, 2, 2, 2, 2, 2, 3] {
            b.extend(raw_guid(day));
        }
        b.extend([0x25, 0xf2, 0x78, 0x41]);
        b.extend(OPEN);
        b.extend(0u32.to_le_bytes());
        b.extend(CLOSE);
        b.extend(OPEN);
        b.extend(2u32.to_le_bytes());
        for (id, name, kind, slots) in [(2, "Working", 1u32, vec![(118800i64, 147600i64)]), (3, "Non-working", 2, Vec::new())] {
            b.extend([0x1d, 0xc2, 0xf4, 0xc4]);
            b.extend(raw_guid(id));
            b.extend(text(name));
            b.extend([0; 12]);
            b.extend(kind.to_le_bytes());
            b.extend((slots.len() as u32).to_le_bytes());
            for (a, z) in slots {
                b.extend(a.to_le_bytes());
                b.extend(z.to_le_bytes());
            }
            b.extend([0x19, 0x06, 0x8d, 0x63]);
            b.extend((id as u32).to_le_bytes());
        }
        b.extend(CLOSE);
        b.extend([0x19, 0xea, 0x70, 0x7d]);
        b.extend(0u32.to_le_bytes());
        b.extend(CLOSE);
        b.extend(raw_guid(1));
        b
    }
    #[test]
    fn binary_framing_truncations_limits_and_trailing_data_are_checked() {
        let b = encoded();
        let parsed = parse(&b).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.records.len(), 1);
        assert_eq!(parsed.records[0].day_types[0].intervals[0].start_second, 9 * 3600);
        for n in 0..b.len() {
            assert!(parse(&b[..n]).is_err(), "cut {n}");
        }
        let mut trailing = b.clone();
        trailing.push(0);
        assert!(parse(&trailing).is_err());
        let mut too_many = b;
        too_many[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(parse(&too_many), Err(FormatError::TooLarge(_))));
    }
    #[test]
    fn paired_native_date_controls_determine_weekday_rotation_and_working_time() {
        let c = data();
        let start = 1_794_474_000;
        let plain = c.working_segments(None, start, 86_400).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(plain.finish_wallclock, 1_794_848_400);
        assert_eq!(plain.segments.len(), 3);
        assert_eq!(plain.segments.iter().map(|s| s.working_seconds).sum::<u64>(), 86_400);
        assert_ne!(plain.finish_wallclock, start + 86_400);
        let mut c = c;
        c.records[0].exceptions.push(DateException { offset: 0, date_wallclock: 1_794_528_000, day_type_guid: guid(3), ordinal_storage: 0 });
        let changed = c.working_segments(None, start, 86_400).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(changed.finish_wallclock, 1_794_934_800);
        assert_eq!(changed.segments.len(), 3);
    }
    #[test]
    fn split_intervals_preserve_breaks_and_zero_duration_is_an_instant() {
        let mut c = data();
        c.records[0].day_types[0].intervals = vec![interval(8 * 3600, 12 * 3600), interval(13 * 3600, 17 * 3600), interval(18 * 3600, 20 * 3600)];
        let result = c.working_segments(None, 8 * 3600, 10 * 3600).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result.finish_wallclock, 20 * 3600);
        assert_eq!(result.segments.len(), 3);
        let midday = c.working_segments(None, 12 * 3600, 2 * 3600).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(midday.first_work_start_wallclock, Some(13 * 3600));
        assert_eq!(midday.finish_wallclock, 15 * 3600);
        let zero = c.working_segments(None, 12 * 3600, 0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(zero.finish_wallclock, 12 * 3600);
        assert!(zero.segments.is_empty());
    }
    #[test]
    fn cancellation_and_operation_budgets_are_typed() {
        let c = data();
        assert_eq!(c.working_segments_with_budget(None, 0, 1, WorkingBudget::default(), || true), Err(CalendarError::Cancelled));
        let mut checks = 0;
        let err = c.working_segments_with_budget(None, 0, 86_400, WorkingBudget::default(), || {
            checks += 1;
            checks > 4
        });
        assert_eq!(err, Err(CalendarError::Cancelled));
        assert!(matches!(
            c.working_segments_with_budget(None, 0, 86_400, WorkingBudget { max_scanned_days: 1, max_segments: 10 }, || false),
            Err(CalendarError::Limit { .. })
        ));
        assert!(matches!(
            c.working_segments_with_budget(None, 9 * 3600, 1, WorkingBudget { max_scanned_days: 1, max_segments: 0 }, || false),
            Err(CalendarError::Limit { .. })
        ));
        assert!(matches!(c.working_segments(None, 0, MAX_WORK_SECONDS + 1), Err(CalendarError::Limit { .. })));
    }
    #[test]
    fn missing_default_parent_cycles_and_invalid_intervals_never_fabricate_finish() {
        let mut c = data();
        c.default_calendar_guid = None;
        assert!(matches!(c.working_segments(None, 0, 1), Err(CalendarError::MissingCalendar { .. })));
        let mut c = data();
        c.records[0].parent_guid = Some(guid(1));
        assert!(matches!(c.working_segments(None, 0, 1), Err(CalendarError::InvalidData { .. })));
        let mut c = data();
        c.records[0].weekday_day_type_guids[4] = None;
        assert!(matches!(c.working_segments(None, 0, 1), Err(CalendarError::Unsupported { .. })));
        let mut c = data();
        c.records[0].day_types[0].intervals.push(interval(10 * 3600, 11 * 3600));
        assert!(matches!(c.working_segments(None, 0, 1), Err(CalendarError::InvalidData { .. })));
        assert!(matches!(data().working_segments(None, UNSPECIFIED_WALLCLOCK, 1), Err(CalendarError::Unsupported { .. })));
    }
    #[test]
    fn unverified_ancestor_exception_inheritance_is_explicit() {
        let mut c = data();
        let mut child = c.records[0].clone();
        child.guid = guid(4);
        child.parent_guid = Some(guid(1));
        child.day_types.clear();
        c.records[0].exceptions.push(DateException { offset: 0, date_wallclock: 0, day_type_guid: guid(3), ordinal_storage: 0 });
        c.records.push(child);
        assert!(matches!(c.working_segments(Some(&guid(4)), 0, 1), Err(CalendarError::Unsupported { .. })));
    }
}
