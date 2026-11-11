//! Pure, bounded schedule and cost simulation using explicit caller-supplied semantics.
//!
//! This module does not decode calendars, infer timezones, reschedule dependencies, or claim
//! parity with any application. Working allocations and effective task bounds are inputs.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_RECORDS: usize = 10_000;
const MAX_ID_BYTES: usize = 256;
const MAX_MINOR: i64 = 1_000_000_000_000;
const MAX_UNITS: u64 = 1_000_000_000_000_000;
const UNIT_SCALE: i128 = 1_000_000;

/// Seconds of a displayed wall clock. This is deliberately not a UTC instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WallClock(pub i64);

/// Half-open interval: the start belongs to the interval; the end does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval {
    pub start: WallClock,
    pub end: WallClock,
}

impl Interval {
    fn validate(self) -> Result<(), SimulationError> {
        if self.start >= self.end {
            return Err(SimulationError::InvalidInterval);
        }
        Ok(())
    }
    fn active(self, focus: WallClock) -> bool {
        self.start <= focus && focus < self.end
    }
    fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Verification is asserted by the supplying adapter, not performed by this evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    CallerDeclared,
    IndependentlyVerified,
}

fn combined(values: impl IntoIterator<Item = Provenance>) -> Provenance {
    if values.into_iter().all(|v| v == Provenance::IndependentlyVerified) { Provenance::IndependentlyVerified } else { Provenance::CallerDeclared }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// Adapter-supplied effective bounds, already accounting for its date/status/calendar choice.
    pub effective: Option<Interval>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultProfile {
    Install,
    Maintain,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub task_id: String,
    pub resource_id: String,
    pub profile: DefaultProfile,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Hidden,
    InstallActive,
    MaintainActive,
    OriginalMaterial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseSemantics {
    /// Any active Install wins; otherwise any completed Install leaves original material visible.
    InstallUnion,
    /// Exactly one Install and one Maintain: active Maintain wins; inactive state follows Install.
    InstallMaintainPair,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedReason {
    NoAssignments,
    MissingEffectiveInterval,
    UnsupportedProfile,
    UnsupportedProfileFamily,
    CoincidentMixedBoundaries,
}

impl UnresolvedReason {
    pub fn remediation(self) -> &'static str {
        match self {
            Self::NoAssignments => "Supply the resource's task assignments.",
            Self::MissingEffectiveInterval => "Supply explicit effective start and finish from a declared date/calendar policy.",
            Self::UnsupportedProfile => "Map only profiles with supported declared semantics; retain other profiles as unresolved.",
            Self::UnsupportedProfileFamily => "Supply all-Install assignments or exactly one Install and one Maintain.",
            Self::CoincidentMixedBoundaries => "Supply an independently established tie policy before evaluating coincident mixed-profile boundaries.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PhaseState {
    Resolved { phase: Phase, semantics: PhaseSemantics },
    Unresolved { reason: UnresolvedReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseOutcome {
    pub state: PhaseState,
    pub provenance: Provenance,
    /// Always false: provenance of inputs does not establish application-wide parity.
    pub native_parity_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationError {
    RecordLimit,
    InvalidId,
    DuplicateId,
    MissingTask,
    DuplicateAssignment,
    InvalidInterval,
    InvalidAmount,
    InvalidUnits,
    ArithmeticOverflow,
    OverlappingRateWindows,
    OverlappingWorkingAllocations,
    MissingRate,
    SplitAllocationAtRateBoundary,
    FixedAllocationUnsupported,
    OverlappingBuckets,
    SplitAllocationAtBucketBoundary,
    MissingBucket,
}

impl SimulationError {
    pub fn remediation(&self) -> &'static str {
        match self {
            Self::RecordLimit => "Reduce each input collection to at most 10,000 records.",
            Self::InvalidId => "Supply nonempty identifiers no longer than 256 bytes.",
            Self::DuplicateId => "Supply unique task, rate, allocation and bucket identifiers.",
            Self::MissingTask => "Supply the task referenced by each selected assignment.",
            Self::DuplicateAssignment => "Remove duplicate task/resource assignments or establish their separate semantics.",
            Self::InvalidInterval => "Supply a start strictly before the finish, excluding missing-date sentinels.",
            Self::InvalidAmount => "Supply nonnegative amounts at most 1,000,000,000,000 minor units.",
            Self::InvalidUnits => "Supply at most 1,000,000,000,000,000 millionths of a unit.",
            Self::ArithmeticOverflow => "Reduce amounts or allocations to fit checked integer arithmetic.",
            Self::OverlappingRateWindows => "Supply nonoverlapping half-open rate windows.",
            Self::OverlappingWorkingAllocations => "Supply nonoverlapping working allocations for one assignment.",
            Self::MissingRate => "Supply a rate window covering each working allocation.",
            Self::SplitAllocationAtRateBoundary => "Split working allocations at rate changes and explicitly allocate units to each part.",
            Self::FixedAllocationUnsupported => "Use Fixed only for constant-rate budget; actual/remaining allocation semantics are unresolved.",
            Self::OverlappingBuckets => "Supply nonoverlapping half-open reporting buckets.",
            Self::SplitAllocationAtBucketBoundary => "Split allocations at reporting boundaries with explicit units before costing.",
            Self::MissingBucket => "Supply a reporting bucket containing each allocation.",
        }
    }
}

fn bounded(n: usize) -> Result<(), SimulationError> {
    if n > MAX_RECORDS { Err(SimulationError::RecordLimit) } else { Ok(()) }
}
fn id(s: &str) -> Result<(), SimulationError> {
    if s.is_empty() || s.len() > MAX_ID_BYTES { Err(SimulationError::InvalidId) } else { Ok(()) }
}

/// Evaluates only the selected resource. Opaque IDs and unrelated fields are never inferred.
pub fn evaluate_resource(tasks: &[Task], assignments: &[Assignment], resource: &str, focus: WallClock) -> Result<PhaseOutcome, SimulationError> {
    bounded(tasks.len())?;
    bounded(assignments.len())?;
    id(resource)?;
    let mut indexed = BTreeMap::new();
    for task in tasks {
        id(&task.id)?;
        if indexed.insert(task.id.as_str(), task).is_some() {
            return Err(SimulationError::DuplicateId);
        }
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut proofs = Vec::new();
    for assignment in assignments {
        id(&assignment.resource_id)?;
        id(&assignment.task_id)?;
        if assignment.resource_id != resource {
            continue;
        }
        if !seen.insert(assignment.task_id.as_str()) {
            return Err(SimulationError::DuplicateAssignment);
        }
        let task = indexed.get(assignment.task_id.as_str()).ok_or(SimulationError::MissingTask)?;
        proofs.extend([task.provenance, assignment.provenance]);
        selected.push((assignment.profile, task.effective));
    }
    let provenance = if selected.is_empty() { Provenance::CallerDeclared } else { combined(proofs) };
    let unresolved = |reason| Ok(PhaseOutcome { state: PhaseState::Unresolved { reason }, provenance, native_parity_verified: false });
    if selected.is_empty() {
        return unresolved(UnresolvedReason::NoAssignments);
    }
    if selected.iter().any(|(p, _)| *p == DefaultProfile::Unsupported) {
        return unresolved(UnresolvedReason::UnsupportedProfile);
    }
    if selected.iter().any(|(_, i)| i.is_none()) {
        return unresolved(UnresolvedReason::MissingEffectiveInterval);
    }
    let mut installs = Vec::new();
    let mut maintains = Vec::new();
    for (profile, interval) in selected {
        let Some(interval) = interval else {
            return unresolved(UnresolvedReason::MissingEffectiveInterval);
        };
        interval.validate()?;
        match profile {
            DefaultProfile::Install => installs.push(interval),
            DefaultProfile::Maintain => maintains.push(interval),
            DefaultProfile::Unsupported => return unresolved(UnresolvedReason::UnsupportedProfile),
        }
    }
    let (phase, semantics) = if maintains.is_empty() {
        let phase = if installs.iter().any(|i| i.active(focus)) {
            Phase::InstallActive
        } else if installs.iter().any(|i| focus >= i.end) {
            Phase::OriginalMaterial
        } else {
            Phase::Hidden
        };
        (phase, PhaseSemantics::InstallUnion)
    } else if installs.len() == 1 && maintains.len() == 1 {
        let Some((install, maintain)) = installs.first().zip(maintains.first()) else {
            return unresolved(UnresolvedReason::UnsupportedProfileFamily);
        };
        if install.start == maintain.start || install.end == maintain.end {
            return unresolved(UnresolvedReason::CoincidentMixedBoundaries);
        }
        let phase = if maintain.active(focus) {
            Phase::MaintainActive
        } else if install.active(focus) {
            Phase::InstallActive
        } else if focus >= install.end {
            Phase::OriginalMaterial
        } else {
            Phase::Hidden
        };
        (phase, PhaseSemantics::InstallMaintainPair)
    } else {
        return unresolved(UnresolvedReason::UnsupportedProfileFamily);
    };
    Ok(PhaseOutcome { state: PhaseState::Resolved { phase, semantics }, provenance, native_parity_verified: false })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    Fixed,
    PerUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSemantics {
    ConstantPerUnit,
    FixedBudgetOnly,
    ExplicitWorkingAllocations,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoneyRounding {
    HalfUpPerAllocation,
}

/// One currency/unit context per evaluation. No currency or engineering-unit conversion occurs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rate {
    pub id: String,
    pub effective: Interval,
    pub kind: CostKind,
    pub amount_minor: i64,
    pub provenance: Provenance,
}

fn validate_rate(rate: &Rate) -> Result<(), SimulationError> {
    id(&rate.id)?;
    rate.effective.validate()?;
    if !(0..=MAX_MINOR).contains(&rate.amount_minor) {
        return Err(SimulationError::InvalidAmount);
    }
    Ok(())
}

/// Round each allocation once to the nearest minor unit, with half values rounded upward.
/// Nonnegative inputs make this equivalent to half away from zero. Never use float money.
fn multiply(amount: i64, units: u64) -> Result<i64, SimulationError> {
    if !(0..=MAX_MINOR).contains(&amount) {
        return Err(SimulationError::InvalidAmount);
    }
    if units > MAX_UNITS {
        return Err(SimulationError::InvalidUnits);
    }
    let product = i128::from(amount).checked_mul(i128::from(units)).ok_or(SimulationError::ArithmeticOverflow)?;
    let rounded = product.checked_add(UNIT_SCALE / 2).ok_or(SimulationError::ArithmeticOverflow)? / UNIT_SCALE;
    i64::try_from(rounded).map_err(|_| SimulationError::ArithmeticOverflow)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstantCostOutcome {
    pub semantics: CostSemantics,
    pub rounding: MoneyRounding,
    pub budget_minor: i64,
    pub actual_minor: Option<i64>,
    pub remaining_minor: Option<i64>,
    pub provenance: Provenance,
    pub native_parity_verified: bool,
    /// Fixed actual/remaining and missing quantities remain unresolved, never zero-filled.
    pub unresolved_actual_remaining: bool,
}

pub fn constant_cost(
    rate: &Rate,
    effective: Interval,
    planned_units: u64,
    actual_units: Option<u64>,
    remaining_units: Option<u64>,
    allocation_provenance: Provenance,
) -> Result<ConstantCostOutcome, SimulationError> {
    validate_rate(rate)?;
    effective.validate()?;
    for units in [Some(planned_units), actual_units, remaining_units].into_iter().flatten() {
        if units > MAX_UNITS {
            return Err(SimulationError::InvalidUnits);
        }
    }
    if !rate.effective.contains(effective) {
        return Err(SimulationError::SplitAllocationAtRateBoundary);
    }
    let (budget, actual, remaining) = match rate.kind {
        CostKind::Fixed => (rate.amount_minor, None, None),
        CostKind::PerUnit => (
            multiply(rate.amount_minor, planned_units)?,
            actual_units.map(|n| multiply(rate.amount_minor, n)).transpose()?,
            remaining_units.map(|n| multiply(rate.amount_minor, n)).transpose()?,
        ),
    };
    let semantics = match rate.kind {
        CostKind::Fixed => CostSemantics::FixedBudgetOnly,
        CostKind::PerUnit => CostSemantics::ConstantPerUnit,
    };
    Ok(ConstantCostOutcome {
        semantics,
        rounding: MoneyRounding::HalfUpPerAllocation,
        budget_minor: budget,
        actual_minor: actual,
        remaining_minor: remaining,
        unresolved_actual_remaining: actual.is_none() || remaining.is_none(),
        provenance: combined([rate.provenance, allocation_provenance]),
        native_parity_verified: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostMeasure {
    Budget,
    Actual,
    Remaining,
}

/// A caller's explicit unit allocation to a working interval for ONE assignment.
/// Split at calendar, rate and reporting boundaries before calling; no interpolation is inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingAllocation {
    pub id: String,
    pub interval: Interval,
    pub units_millionths: u64,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bucket {
    pub id: String,
    pub interval: Interval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostLine {
    pub allocation_id: String,
    pub rate_id: String,
    pub bucket_id: Option<String>,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketCost {
    pub bucket_id: String,
    pub interval_minor: i64,
    pub cumulative_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocatedCostOutcome {
    pub semantics: CostSemantics,
    pub rounding: MoneyRounding,
    pub measure: CostMeasure,
    pub lines: Vec<CostLine>,
    /// Leaf allocation lines only; no task-parent/resource-child rollups are accepted as inputs.
    pub total_minor: i64,
    pub buckets: Vec<BucketCost>,
    pub provenance: Provenance,
    pub native_parity_verified: bool,
}

/// Costs explicit nonoverlapping working allocations using explicit rate windows.
/// Buckets, when supplied, must fully contain each allocation. Rounding occurs per allocation;
/// cumulative values sum those rounded lines, so displayed interval values always reconcile.
pub fn allocated_cost(
    rates: &[Rate],
    allocations: &[WorkingAllocation],
    buckets: &[Bucket],
    measure: CostMeasure,
) -> Result<AllocatedCostOutcome, SimulationError> {
    bounded(rates.len())?;
    bounded(allocations.len())?;
    bounded(buckets.len())?;
    // A product cap bounds the simple interval lookup independently of collection limits.
    if rates.len().checked_mul(allocations.len()).ok_or(SimulationError::RecordLimit)? > 1_000_000
        || buckets.len().checked_mul(allocations.len()).ok_or(SimulationError::RecordLimit)? > 1_000_000
    {
        return Err(SimulationError::RecordLimit);
    }
    let mut sorted_rates: Vec<_> = rates.iter().collect();
    let mut seen = BTreeSet::new();
    for rate in &sorted_rates {
        validate_rate(rate)?;
        if !seen.insert(&rate.id) {
            return Err(SimulationError::DuplicateId);
        }
    }
    sorted_rates.sort_by_key(|r| r.effective.start);
    if sorted_rates.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| a.effective.end > b.effective.start)) {
        return Err(SimulationError::OverlappingRateWindows);
    }
    let mut sorted_allocations: Vec<_> = allocations.iter().collect();
    seen.clear();
    for a in &sorted_allocations {
        id(&a.id)?;
        a.interval.validate()?;
        if a.units_millionths > MAX_UNITS {
            return Err(SimulationError::InvalidUnits);
        }
        if !seen.insert(&a.id) {
            return Err(SimulationError::DuplicateId);
        }
    }
    sorted_allocations.sort_by_key(|a| a.interval.start);
    if sorted_allocations.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| a.interval.end > b.interval.start)) {
        return Err(SimulationError::OverlappingWorkingAllocations);
    }
    let mut sorted_buckets: Vec<_> = buckets.iter().collect();
    seen.clear();
    for b in &sorted_buckets {
        id(&b.id)?;
        b.interval.validate()?;
        if !seen.insert(&b.id) {
            return Err(SimulationError::DuplicateId);
        }
    }
    sorted_buckets.sort_by_key(|b| b.interval.start);
    if sorted_buckets.windows(2).any(|w| w.first().zip(w.get(1)).is_some_and(|(a, b)| a.interval.end > b.interval.start)) {
        return Err(SimulationError::OverlappingBuckets);
    }
    let mut lines = Vec::new();
    let mut total = 0i64;
    let mut sums = BTreeMap::<String, i64>::new();
    let mut proofs = Vec::new();
    for a in sorted_allocations {
        let rate = sorted_rates.iter().find(|r| r.effective.active(a.interval.start)).ok_or(SimulationError::MissingRate)?;
        if !rate.effective.contains(a.interval) {
            return Err(SimulationError::SplitAllocationAtRateBoundary);
        }
        if rate.kind != CostKind::PerUnit {
            return Err(SimulationError::FixedAllocationUnsupported);
        }
        let amount = multiply(rate.amount_minor, a.units_millionths)?;
        let bucket_id = if sorted_buckets.is_empty() {
            None
        } else {
            let bucket = sorted_buckets.iter().find(|b| b.interval.active(a.interval.start)).ok_or(SimulationError::MissingBucket)?;
            if !bucket.interval.contains(a.interval) {
                return Err(SimulationError::SplitAllocationAtBucketBoundary);
            }
            let sum = sums.entry(bucket.id.clone()).or_default();
            *sum = sum.checked_add(amount).ok_or(SimulationError::ArithmeticOverflow)?;
            Some(bucket.id.clone())
        };
        total = total.checked_add(amount).ok_or(SimulationError::ArithmeticOverflow)?;
        proofs.extend([a.provenance, rate.provenance]);
        lines.push(CostLine { allocation_id: a.id.clone(), rate_id: rate.id.clone(), bucket_id, amount_minor: amount });
    }
    let mut cumulative = 0i64;
    let mut output_buckets = Vec::new();
    for b in sorted_buckets {
        let interval = sums.get(&b.id).copied().unwrap_or_default();
        cumulative = cumulative.checked_add(interval).ok_or(SimulationError::ArithmeticOverflow)?;
        output_buckets.push(BucketCost { bucket_id: b.id.clone(), interval_minor: interval, cumulative_minor: cumulative });
    }
    let provenance = if lines.is_empty() { Provenance::CallerDeclared } else { combined(proofs) };
    Ok(AllocatedCostOutcome {
        semantics: CostSemantics::ExplicitWorkingAllocations,
        rounding: MoneyRounding::HalfUpPerAllocation,
        measure,
        lines,
        total_minor: total,
        buckets: output_buckets,
        provenance,
        native_parity_verified: false,
    })
}

#[cfg(test)]
mod tests;
