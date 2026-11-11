use super::*;

const DECLARED: Provenance = Provenance::CallerDeclared;
const VERIFIED: Provenance = Provenance::IndependentlyVerified;
fn interval(start: i64, end: i64) -> Interval {
    Interval { start: WallClock(start), end: WallClock(end) }
}
fn task(name: &str, start: i64, end: i64) -> Task {
    Task { id: name.into(), effective: Some(interval(start, end)), provenance: DECLARED }
}
fn assignment(name: &str, profile: DefaultProfile) -> Assignment {
    Assignment { task_id: name.into(), resource_id: "part".into(), profile, provenance: DECLARED }
}
fn resolved(tasks: &[Task], assignments: &[Assignment], t: i64) -> Phase {
    match evaluate_resource(tasks, assignments, "part", WallClock(t)).expect("valid synthetic inputs").state {
        PhaseState::Resolved { phase, .. } => phase,
        other => panic!("expected resolved phase: {other:?}"),
    }
}
fn rate(name: &str, start: i64, end: i64, amount: i64) -> Rate {
    Rate { id: name.into(), effective: interval(start, end), kind: CostKind::PerUnit, amount_minor: amount, provenance: DECLARED }
}
fn allocation(name: &str, start: i64, end: i64, units: u64) -> WorkingAllocation {
    WorkingAllocation { id: name.into(), interval: interval(start, end), units_millionths: units, provenance: DECLARED }
}

#[test]
fn install_start_and_finish_are_half_open() {
    let tasks = [task("build", -20, 40)];
    let assignments = [assignment("build", DefaultProfile::Install)];
    assert_eq!(resolved(&tasks, &assignments, -21), Phase::Hidden);
    assert_eq!(resolved(&tasks, &assignments, -20), Phase::InstallActive);
    assert_eq!(resolved(&tasks, &assignments, 39), Phase::InstallActive);
    assert_eq!(resolved(&tasks, &assignments, 40), Phase::OriginalMaterial);
}

#[test]
fn install_union_keeps_material_during_gap_and_active_wins_overlap() {
    let tasks = [task("first", 0, 10), task("later", 20, 30), task("overlap", 5, 15)];
    let assignments = tasks.iter().map(|t| assignment(&t.id, DefaultProfile::Install)).collect::<Vec<_>>();
    assert_eq!(resolved(&tasks, &assignments, -1), Phase::Hidden);
    assert_eq!(resolved(&tasks, &assignments, 10), Phase::InstallActive);
    assert_eq!(resolved(&tasks, &assignments, 18), Phase::OriginalMaterial);
    assert_eq!(resolved(&tasks, &assignments, 20), Phase::InstallActive);
    assert_eq!(resolved(&tasks, &assignments, 30), Phase::OriginalMaterial);
}

#[test]
fn maintain_active_precedence_is_profile_based_and_order_independent() {
    let tasks = [task("install", 20, 60), task("service", 10, 50)];
    let mut assignments = [assignment("install", DefaultProfile::Install), assignment("service", DefaultProfile::Maintain)];
    for _ in 0..2 {
        assert_eq!(resolved(&tasks, &assignments, 9), Phase::Hidden);
        assert_eq!(resolved(&tasks, &assignments, 10), Phase::MaintainActive);
        assert_eq!(resolved(&tasks, &assignments, 30), Phase::MaintainActive);
        assert_eq!(resolved(&tasks, &assignments, 50), Phase::InstallActive);
        assert_eq!(resolved(&tasks, &assignments, 60), Phase::OriginalMaterial);
        assignments.reverse();
    }
}

#[test]
fn unsupported_families_and_missing_dates_remain_actionable() {
    let tasks = [task("service", 1, 3)];
    let out = evaluate_resource(&tasks, &[assignment("service", DefaultProfile::Maintain)], "part", WallClock(2)).unwrap();
    assert_eq!(out.state, PhaseState::Unresolved { reason: UnresolvedReason::UnsupportedProfileFamily });
    let missing = Task { effective: None, ..tasks[0].clone() };
    let out = evaluate_resource(&[missing], &[assignment("service", DefaultProfile::Install)], "part", WallClock(2)).unwrap();
    assert_eq!(out.state, PhaseState::Unresolved { reason: UnresolvedReason::MissingEffectiveInterval });
    assert!(!UnresolvedReason::MissingEffectiveInterval.remediation().is_empty());
    let out = evaluate_resource(&tasks, &[assignment("service", DefaultProfile::Unsupported)], "part", WallClock(2)).unwrap();
    assert_eq!(out.state, PhaseState::Unresolved { reason: UnresolvedReason::UnsupportedProfile });
}

#[test]
fn coincident_mixed_boundaries_are_not_arbitrarily_ranked() {
    let tasks = [task("a", 0, 10), task("b", 0, 20)];
    let assignments = [assignment("a", DefaultProfile::Install), assignment("b", DefaultProfile::Maintain)];
    assert_eq!(
        evaluate_resource(&tasks, &assignments, "part", WallClock(2)).unwrap().state,
        PhaseState::Unresolved { reason: UnresolvedReason::CoincidentMixedBoundaries }
    );
}

#[test]
fn effective_interval_is_used_without_interpreting_status_or_calendar() {
    // An adapter has already selected its actual/current bounds; no planned-duration addition.
    let tasks = [task("current", 100, 109)];
    let assignments = [assignment("current", DefaultProfile::Install)];
    assert_eq!(resolved(&tasks, &assignments, 99), Phase::Hidden);
    assert_eq!(resolved(&tasks, &assignments, 105), Phase::InstallActive);
    assert_eq!(resolved(&tasks, &assignments, 109), Phase::OriginalMaterial);
}

#[test]
fn provenance_never_promotes_input_verification_to_global_native_parity() {
    let tasks = [Task { provenance: VERIFIED, ..task("a", 0, 2) }];
    let assignments = [Assignment { provenance: VERIFIED, ..assignment("a", DefaultProfile::Install) }];
    let out = evaluate_resource(&tasks, &assignments, "part", WallClock(1)).unwrap();
    assert_eq!(out.provenance, VERIFIED);
    assert!(!out.native_parity_verified);
    let out = evaluate_resource(&tasks, &[assignment("a", DefaultProfile::Install)], "part", WallClock(1)).unwrap();
    assert_eq!(out.provenance, DECLARED);
    let no_assignments = evaluate_resource(&tasks, &[], "part", WallClock(1)).unwrap();
    assert_eq!(no_assignments.provenance, DECLARED);
}

#[test]
fn malformed_ids_references_intervals_and_duplicate_assignments_fail() {
    let tasks = [task("a", 0, 3)];
    let a = assignment("a", DefaultProfile::Install);
    assert_eq!(evaluate_resource(&tasks, &[a.clone(), a], "part", WallClock(1)), Err(SimulationError::DuplicateAssignment));
    assert_eq!(evaluate_resource(&tasks, &[assignment("absent", DefaultProfile::Install)], "part", WallClock(1)), Err(SimulationError::MissingTask));
    assert_eq!(evaluate_resource(&[task("a", 3, 3)], &[assignment("a", DefaultProfile::Install)], "part", WallClock(3)), Err(SimulationError::InvalidInterval));
    assert_eq!(evaluate_resource(&tasks, &[], "", WallClock(0)), Err(SimulationError::InvalidId));
    assert_eq!(evaluate_resource(&[tasks[0].clone(), tasks[0].clone()], &[], "part", WallClock(0)), Err(SimulationError::DuplicateId));
}

#[test]
fn constant_per_unit_has_separate_planned_actual_remaining() {
    let out = constant_cost(&rate("r", 0, 100, 215), interval(10, 20), 4_000_000, Some(1_000_000), Some(3_000_000), DECLARED).unwrap();
    assert_eq!((out.budget_minor, out.actual_minor, out.remaining_minor), (860, Some(215), Some(645)));
    assert!(!out.unresolved_actual_remaining);
    assert!(!out.native_parity_verified);
}

#[test]
fn fixed_budget_does_not_fabricate_actual_or_remaining() {
    let fixed = Rate { kind: CostKind::Fixed, ..rate("fixed", 0, 100, 87) };
    let out = constant_cost(&fixed, interval(10, 20), 9_000_000, Some(2_000_000), Some(7_000_000), DECLARED).unwrap();
    assert_eq!(out.budget_minor, 87);
    assert_eq!(out.actual_minor, None);
    assert_eq!(out.remaining_minor, None);
    assert!(out.unresolved_actual_remaining);
    let out = constant_cost(&rate("linear", 0, 100, 50), interval(10, 20), 1_000_000, None, None, DECLARED).unwrap();
    assert_eq!(out.actual_minor, None);
    assert_eq!(out.remaining_minor, None);
}

#[test]
fn integer_rounding_is_half_up_per_explicit_allocation() {
    assert_eq!(multiply(1, 499_999), Ok(0));
    assert_eq!(multiply(1, 500_000), Ok(1));
    assert_eq!(multiply(3, 500_000), Ok(2));
    assert_eq!(multiply(0, MAX_UNITS), Ok(0));
    assert_eq!(multiply(-1, 1), Err(SimulationError::InvalidAmount));
    assert_eq!(multiply(1, MAX_UNITS + 1), Err(SimulationError::InvalidUnits));
    assert_eq!(multiply(MAX_MINOR, MAX_UNITS), Err(SimulationError::ArithmeticOverflow));
}

#[test]
fn rate_crossings_require_explicit_allocation_split() {
    let rates = [rate("early", 0, 10, 120), rate("later", 10, 30, 240)];
    let rows = [allocation("one", 1, 10, 1_000_000), allocation("two", 10, 20, 2_000_000)];
    let out = allocated_cost(&rates, &rows, &[], CostMeasure::Budget).unwrap();
    assert_eq!(out.total_minor, 600);
    assert_eq!(out.lines[1].rate_id, "later");
    assert_eq!(allocated_cost(&rates, &[allocation("cross", 5, 15, 3_000_000)], &[], CostMeasure::Budget), Err(SimulationError::SplitAllocationAtRateBoundary));
    assert_eq!(constant_cost(&rates[0], interval(5, 15), 1, None, None, DECLARED), Err(SimulationError::SplitAllocationAtRateBoundary));
}

#[test]
fn buckets_reconcile_rounded_lines_without_duplicate_rollup() {
    let rows = [allocation("x", 1, 4, 500_000), allocation("y", 11, 14, 500_000)];
    let buckets = [
        Bucket { id: "later".into(), interval: interval(10, 20) },
        Bucket { id: "first".into(), interval: interval(0, 10) },
        Bucket { id: "empty".into(), interval: interval(20, 30) },
    ];
    let out = allocated_cost(&[rate("r", 0, 30, 1)], &rows, &buckets, CostMeasure::Actual).unwrap();
    assert_eq!(out.total_minor, 2);
    assert_eq!(out.lines.len(), 2);
    assert_eq!(out.buckets[0].bucket_id, "first");
    assert_eq!(out.buckets.iter().map(|b| (b.interval_minor, b.cumulative_minor)).collect::<Vec<_>>(), [(1, 1), (1, 2), (0, 2)]);
    assert_eq!(out.measure, CostMeasure::Actual);
}

#[test]
fn reporting_boundaries_and_rate_gaps_are_actionable_errors() {
    let r = [rate("r", 0, 100, 100)];
    let row = [allocation("cross", 5, 15, 1_000_000)];
    let buckets = [Bucket { id: "b".into(), interval: interval(0, 10) }];
    assert_eq!(allocated_cost(&r, &row, &buckets, CostMeasure::Budget), Err(SimulationError::SplitAllocationAtBucketBoundary));
    assert_eq!(allocated_cost(&[rate("future", 20, 100, 100)], &row, &[], CostMeasure::Budget), Err(SimulationError::MissingRate));
    assert_eq!(allocated_cost(&r, &[allocation("later", 20, 30, 1)], &buckets, CostMeasure::Budget), Err(SimulationError::MissingBucket));
}

#[test]
fn overlap_duplicate_and_fixed_allocations_are_rejected() {
    let rows = [allocation("a", 0, 10, 1), allocation("b", 5, 15, 1)];
    assert_eq!(allocated_cost(&[rate("r", 0, 30, 1)], &rows, &[], CostMeasure::Remaining), Err(SimulationError::OverlappingWorkingAllocations));
    assert_eq!(allocated_cost(&[rate("r", 0, 20, 1), rate("s", 10, 30, 2)], &[], &[], CostMeasure::Budget), Err(SimulationError::OverlappingRateWindows));
    let fixed = Rate { kind: CostKind::Fixed, ..rate("fixed", 0, 30, 1) };
    assert_eq!(allocated_cost(&[fixed], &rows[..1], &[], CostMeasure::Budget), Err(SimulationError::FixedAllocationUnsupported));
    let dup = [allocation("a", 0, 10, 1), allocation("a", 10, 20, 1)];
    assert_eq!(allocated_cost(&[rate("r", 0, 30, 1)], &dup, &[], CostMeasure::Budget), Err(SimulationError::DuplicateId));
}

#[test]
fn collection_and_product_budgets_bound_work() {
    let tasks = vec![task("a", 0, 1); MAX_RECORDS + 1];
    assert_eq!(evaluate_resource(&tasks, &[], "part", WallClock(0)), Err(SimulationError::RecordLimit));
    let rates = vec![rate("a", 0, 2, 1); 1001];
    let rows = vec![allocation("x", 0, 1, 1); 1000];
    assert_eq!(allocated_cost(&rates, &rows, &[], CostMeasure::Budget), Err(SimulationError::RecordLimit));
}

#[test]
fn outcome_and_errors_roundtrip_as_structured_data() {
    let out = constant_cost(&rate("r", 0, 10, 125), interval(1, 2), 1_000_000, None, None, VERIFIED).unwrap();
    let encoded = serde_json::to_value(&out).unwrap();
    assert_eq!(encoded["provenance"], "caller_declared");
    assert_eq!(serde_json::from_value::<ConstantCostOutcome>(encoded).unwrap(), out);
    assert!(!SimulationError::MissingRate.remediation().is_empty());
}
