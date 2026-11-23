use opsworld_sim::agent::{Agent, Policy};
use opsworld_sim::faults::{generate_schedule, Drift, Fault};
use opsworld_sim::invariants::Invariant;
use opsworld_sim::model::{DocKind, Document, ShipmentId};
use opsworld_sim::render::shrink_lines;
use opsworld_sim::repro::{shrink, Repro};
use opsworld_sim::scenario::{Scenario, ScenarioKind};
use opsworld_sim::sim::{run, run_in, run_seed, run_seed_in, run_with, EventKind, RunResult};
use opsworld_sim::tools::{ToolCall, ToolPort, WriteKind};

const SUITE: u64 = 2000;

fn fired(result: &RunResult, invariant: Invariant) -> bool {
    result.violations.iter().any(|v| v.invariant == invariant)
}

fn first_with(kind: DocKind) -> (u64, ShipmentId) {
    (1..SUITE)
        .find_map(|seed| {
            let scenario = Scenario::generate(seed);
            scenario
                .shipment_ids()
                .into_iter()
                .find(|s| scenario.has_doc(*s, kind) && ships_in_full(&scenario, *s))
                .map(|s| (seed, s))
        })
        .expect("suite contains this document kind")
}

/// Short shipments never get a confirmed promise, so promise faults need a full one.
fn ships_in_full(scenario: &Scenario, shipment: ShipmentId) -> bool {
    scenario.documents.iter().any(|d| {
        d.shipment == shipment
            && d.kind == DocKind::Asn
            && d.field("qty_shipped") == Some(scenario.world.erp.pos[&shipment].ordered)
    })
}

#[test]
fn same_seed_same_trace_hash() {
    for policy in [Policy::V1, Policy::V2] {
        for seed in 1..=300 {
            let (a, b) = (run_seed(seed, policy), run_seed(seed, policy));
            assert_eq!(a.hash, b.hash, "seed {seed}");
            assert_eq!(a.trace, b.trace, "seed {seed}");
        }
    }
}

#[test]
fn different_seeds_produce_different_traces() {
    let hashes: std::collections::BTreeSet<_> =
        (1..=300).map(|s| run_seed(s, Policy::V2).hash).collect();
    assert_eq!(hashes.len(), 300);
}

#[test]
fn fault_listing_order_does_not_change_the_run() {
    for seed in 1..=300 {
        let scenario = Scenario::generate(seed);
        let faults = generate_schedule(&scenario);
        let mut reversed = faults.clone();
        reversed.reverse();
        let a = run_with(seed, &faults, Policy::V1);
        let b = run_with(seed, &reversed, Policy::V1);
        assert_eq!(a.hash, b.hash, "seed {seed}");
    }
}

/// Seed 10 is the seed used in DEMO.md.
#[test]
fn v1_double_applies_after_timeout_on_known_seed() {
    let result = run_seed(10, Policy::V1);
    assert!(result.faults.iter().any(|f| matches!(
        f,
        Fault::TimeoutAfterCommit {
            tool: WriteKind::PostReceipt,
            ..
        }
    )));
    assert!(fired(&result, Invariant::NoBlindRetry));
    assert!(fired(&result, Invariant::NoDoubleApply));
    assert!(run_with(10, &result.faults, Policy::V2).passed());
}

#[test]
fn v1_passes_without_faults() {
    for seed in 1..=SUITE {
        let result = run_with(seed, &[], Policy::V1);
        assert!(result.passed(), "seed {seed}: {:?}", result.violations);
    }
}

#[test]
fn v1_fails_some_seeds_and_v2_passes_the_same_suite() {
    let mut v1_failures = 0;
    for seed in 1..=SUITE {
        v1_failures += u32::from(!run_seed(seed, Policy::V1).passed());
        let v2 = run_seed(seed, Policy::V2);
        assert!(v2.passed(), "seed {seed}: {:?}", v2.violations);
    }
    assert!(v1_failures > 0);
}

#[test]
fn v2_survives_heavy_fault_combinations() {
    for seed in 1..=SUITE {
        let scenario = Scenario::generate(seed);
        let mut faults = Vec::new();
        for offset in 0..6 {
            let mut donor = Scenario::generate(seed + offset);
            donor.seed = seed + offset;
            faults.extend(generate_schedule(&donor));
        }
        faults.retain(|f| fault_applies(f, &scenario));
        let result = run_with(seed, &faults, Policy::V2);
        assert!(result.passed(), "seed {seed}: {:?}", result.violations);
    }
}

fn fault_applies(fault: &Fault, scenario: &Scenario) -> bool {
    let ids = scenario.shipment_ids();
    match fault {
        Fault::CarrierNoShow { shipment }
        | Fault::OutOfOrder { shipment }
        | Fault::LateAsn { shipment, .. }
        | Fault::SchemaDrift { shipment, .. }
        | Fault::DuplicateEdi { shipment, .. } => ids.contains(shipment),
        _ => true,
    }
}

#[test]
fn v2_escalates_only_for_schema_breaks_and_finishes_the_work() {
    for seed in 1..=SUITE {
        let scenario = Scenario::generate(seed);
        let faults = generate_schedule(&scenario);
        let result = run_with(seed, &faults, Policy::V2);
        let drifted = faults
            .iter()
            .any(|f| matches!(f, Fault::SchemaDrift { .. }));
        let escalations = result
            .trace
            .iter()
            .filter(|e| e.kind == EventKind::Escalate)
            .count();
        if !drifted {
            assert_eq!(
                escalations, 0,
                "seed {seed} escalated without a schema break"
            );
        }
        let receipts = result
            .trace
            .iter()
            .filter(|e| e.text.contains("erp.post_receipt"))
            .count();
        if !drifted {
            assert!(receipts >= scenario.shipment_ids().len(), "seed {seed}");
        }
    }
}

fn single_fault_cases() -> Vec<(&'static str, u64, Fault, Invariant)> {
    let (delayed_seed, delayed) = first_with(DocKind::Delayed);
    let (s_seed, s) = first_with(DocKind::Arrived);
    vec![
        (
            "schema drift",
            s_seed,
            Fault::SchemaDrift {
                shipment: s,
                doc: DocKind::Arrived,
                field: "qty_received".into(),
                drift: Drift::Renamed {
                    to: "receivedQty".into(),
                },
            },
            Invariant::NoWriteAfterSchemaBreak,
        ),
        (
            "duplicate edi",
            s_seed,
            Fault::DuplicateEdi {
                shipment: s,
                doc: DocKind::Arrived,
                after: 10,
            },
            Invariant::NoDoubleApply,
        ),
        (
            "out of order",
            delayed_seed,
            Fault::OutOfOrder { shipment: delayed },
            Invariant::PromiseFeasible,
        ),
        (
            "timeout after commit",
            s_seed,
            Fault::TimeoutAfterCommit {
                tool: WriteKind::BookDock,
                nth: 0,
            },
            Invariant::NoDoubleBook,
        ),
        (
            "timeout before commit",
            s_seed,
            Fault::TimeoutBeforeCommit {
                tool: WriteKind::PostReceipt,
                nth: 0,
            },
            Invariant::NoBlindRetry,
        ),
    ]
}

#[test]
fn each_fault_alone_trips_v1_and_not_v2() {
    for (name, seed, fault, invariant) in single_fault_cases() {
        let faults = [fault];
        let v1 = run_with(seed, &faults, Policy::V1);
        assert!(fired(&v1, invariant), "{name}: {:?}", v1.violations);
        let v2 = run_with(seed, &faults, Policy::V2);
        assert!(v2.passed(), "{name}: {:?}", v2.violations);
    }
}

/// v1 is a fair baseline: these two faults alone are within what it handles.
#[test]
fn v1_handles_a_late_asn_or_no_show_on_its_own() {
    let (seed, s) = first_with(DocKind::Arrived);
    for fault in [
        Fault::LateAsn {
            shipment: s,
            delay: 30,
        },
        Fault::CarrierNoShow { shipment: s },
    ] {
        let name = fault.name();
        let faults = [fault];
        let v1 = run_with(seed, &faults, Policy::V1);
        assert!(v1.passed(), "{name}: {:?}", v1.violations);
        assert!(run_with(seed, &faults, Policy::V2).passed(), "{name}");
    }
}

#[test]
fn repro_round_trip_reproduces_the_exact_trace() {
    let dir = std::env::temp_dir().join(format!("opsworld-test-{}", std::process::id()));
    let mut checked = 0;
    for seed in 1..=200 {
        let result = run_seed(seed, Policy::V1);
        let Some(first) = result.violations.first() else {
            continue;
        };
        let repro = Repro::from_failure(&result, Policy::V1, first.invariant);
        let path = repro.save(&dir).unwrap();
        let loaded = Repro::load(&path).unwrap();
        assert_eq!(loaded, repro);
        let replayed = run_with(loaded.seed, &loaded.faults, loaded.policy);
        assert_eq!(replayed.hash, loaded.trace_hash, "seed {seed}");
        checked += 1;
    }
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(checked > 0);
}

#[test]
fn shrunk_schedules_still_fire_and_are_one_minimal() {
    for seed in 1..=300 {
        let result = run_seed(seed, Policy::V1);
        let Some(first) = result.violations.first() else {
            continue;
        };
        let target = first.invariant;
        let minimal = shrink(seed, &result.faults, Policy::V1, target);
        assert!(!minimal.is_empty(), "seed {seed} failed with no faults");
        assert!(fired(&run_with(seed, &minimal, Policy::V1), target));
        for i in 0..minimal.len() {
            let mut fewer = minimal.clone();
            fewer.remove(i);
            assert!(
                !fired(&run_with(seed, &fewer, Policy::V1), target),
                "seed {seed}"
            );
        }
    }
}

/// Treats every timeout as success. The oracle must catch this even though it
/// never retries, which is why lost writes are part of the fault model.
struct AssumesCommitted;

impl Agent for AssumesCommitted {
    fn name(&self) -> &'static str {
        "assumes-committed"
    }

    fn handle(&mut self, doc: &Document, port: &mut dyn ToolPort) {
        if doc.kind == DocKind::Arrived {
            let qty = doc.field("qty_received").unwrap();
            let _ = port.call(ToolCall::PostReceipt {
                shipment: doc.shipment,
                qty,
            });
        }
    }
}

#[test]
fn oracle_catches_an_agent_that_assumes_timeouts_committed() {
    let scenario = Scenario::generate(7);
    assert!(run(&scenario, &[], &mut AssumesCommitted).passed());
    let lost = [Fault::TimeoutBeforeCommit {
        tool: WriteKind::PostReceipt,
        nth: 0,
    }];
    let result = run(&scenario, &lost, &mut AssumesCommitted);
    assert!(fired(&result, Invariant::QuantitiesReconcile));
}

#[test]
fn recorded_shrink_is_minimal_and_printed_before_and_after() {
    let mut reduced = 0;
    for seed in 1..=300 {
        let result = run_seed(seed, Policy::V1);
        let Some(first) = result.violations.first() else {
            continue;
        };
        let target = first.invariant;
        let repro = Repro::from_failure(&result, Policy::V1, target);
        assert_eq!(repro.faults_before, result.faults, "seed {seed}");
        assert!(fired(&run_with(seed, &repro.faults, Policy::V1), target));
        for i in 0..repro.faults.len() {
            let mut fewer = repro.faults.clone();
            fewer.remove(i);
            assert!(!fired(&run_with(seed, &fewer, Policy::V1), target));
        }

        let lines = shrink_lines(&repro);
        let split = lines.iter().position(|l| l == "  after:").unwrap();
        assert!(lines[0].contains(&format!(
            "{} -> {}",
            if repro.faults_before.len() == 1 {
                "1 fault".to_string()
            } else {
                format!("{} faults", repro.faults_before.len())
            },
            repro.faults.len()
        )));
        let printed = |part: &[String]| -> Vec<String> {
            part.iter().map(|l| l.trim().to_string()).collect()
        };
        let text =
            |faults: &[Fault]| -> Vec<String> { faults.iter().map(ToString::to_string).collect() };
        assert_eq!(printed(&lines[2..split]), text(&repro.faults_before));
        assert_eq!(printed(&lines[split + 1..]), text(&repro.faults));
        reduced += u32::from(repro.faults.len() < repro.faults_before.len());
    }
    assert!(reduced > 0, "no schedule in the suite was reduced");
}

#[test]
fn asn_856_documents_carry_edi_fields() {
    let scenario = Scenario::build(ScenarioKind::Asn856, 10);
    let asn = scenario
        .documents
        .iter()
        .find(|d| d.kind == DocKind::Asn)
        .unwrap();
    assert_eq!(asn.fields["transaction_set"], "856");
    for key in ["asn", "po", "line", "scac", "qty_shipped", "eta"] {
        assert!(asn.fields.contains_key(key), "{key}");
    }
    assert_eq!(asn.fields["requested_dock_appt"], asn.fields["eta"]);
    assert!(scenario
        .documents
        .iter()
        .filter(|d| d.kind != DocKind::Asn)
        .all(|d| d.fields["transaction_set"] == "214"));
}

#[test]
fn asn_856_is_deterministic_and_v2_holds() {
    let mut v1_failures = 0;
    for seed in 1..=500 {
        let a = run_seed_in(ScenarioKind::Asn856, seed, Policy::V1);
        let b = run_in(ScenarioKind::Asn856, seed, &a.faults, Policy::V1);
        assert_eq!(a.hash, b.hash, "seed {seed}");
        v1_failures += u32::from(!a.passed());
        let v2 = run_seed_in(ScenarioKind::Asn856, seed, Policy::V2);
        assert!(v2.passed(), "seed {seed}: {:?}", v2.violations);
    }
    assert!(v1_failures > 0);
}

#[test]
fn asn_856_repro_replays_in_its_own_scenario() {
    let result = run_seed_in(ScenarioKind::Asn856, 10, Policy::V1);
    let target = result.violations[0].invariant;
    let repro = Repro::from_failure(&result, Policy::V1, target);
    assert_eq!(repro.scenario, ScenarioKind::Asn856);
    let replayed = run_in(repro.scenario, repro.seed, &repro.faults, repro.policy);
    assert_eq!(replayed.hash, repro.trace_hash);
}
