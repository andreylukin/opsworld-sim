//! Fault types, seeded schedules, and the transforms applied to the feed.

use crate::model::{DocKind, Document, Minutes, ShipmentId};
use crate::rng::Rng;
use crate::scenario::{make_doc, Scenario};
use crate::tools::WriteKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

const FAULT_STREAM: u64 = 2;
/// How long a no-show truck takes to report a recovery ETA.
const NO_SHOW_REPORT: Minutes = 120;
const NO_SHOW_RECOVERY: Minutes = 360;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Drift {
    Renamed { to: String },
    Missing,
}

/// One injected fault. Variant order is the order feed faults are applied in,
/// so a schedule's effect does not depend on how it is listed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "fault")]
pub enum Fault {
    CarrierNoShow {
        shipment: ShipmentId,
    },
    OutOfOrder {
        shipment: ShipmentId,
    },
    LateAsn {
        shipment: ShipmentId,
        delay: Minutes,
    },
    SchemaDrift {
        shipment: ShipmentId,
        doc: DocKind,
        field: String,
        drift: Drift,
    },
    DuplicateEdi {
        shipment: ShipmentId,
        doc: DocKind,
        after: Minutes,
    },
    TimeoutAfterCommit {
        tool: WriteKind,
        nth: u32,
    },
    TimeoutBeforeCommit {
        tool: WriteKind,
        nth: u32,
    },
}

impl Fault {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CarrierNoShow { .. } => "carrier-no-show",
            Self::OutOfOrder { .. } => "out-of-order",
            Self::LateAsn { .. } => "late-asn",
            Self::SchemaDrift { .. } => "schema-drift",
            Self::DuplicateEdi { .. } => "duplicate-edi",
            Self::TimeoutAfterCommit { .. } => "timeout-after-commit",
            Self::TimeoutBeforeCommit { .. } => "timeout-before-commit",
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.name();
        match self {
            Self::CarrierNoShow { shipment } | Self::OutOfOrder { shipment } => {
                write!(f, "{name} {shipment}")
            }
            Self::LateAsn { shipment, delay } => {
                write!(f, "{name} {shipment} +{delay}m after arrival")
            }
            Self::SchemaDrift {
                shipment,
                doc,
                field,
                drift,
            } => match drift {
                Drift::Renamed { to } => {
                    write!(f, "{name} {shipment} {doc}: {field} renamed to {to}")
                }
                Drift::Missing => write!(f, "{name} {shipment} {doc}: {field} missing"),
            },
            Self::DuplicateEdi {
                shipment,
                doc,
                after,
            } => write!(f, "{name} {shipment} {doc} +{after}m"),
            Self::TimeoutAfterCommit { tool, nth } | Self::TimeoutBeforeCommit { tool, nth } => {
                write!(f, "{name} {tool} call #{}", nth + 1)
            }
        }
    }
}

/// Generates the schedule for a seed. It depends only on the seed and the
/// scenario, never on the policy, so every policy faces the same faults.
pub fn generate_schedule(scenario: &Scenario) -> Vec<Fault> {
    let mut rng = Rng::fork(scenario.seed, FAULT_STREAM);
    let ids = scenario.shipment_ids();
    let count = rng.below(4);
    let mut faults = Vec::new();
    for _ in 0..count {
        let s = ids[rng.below(ids.len() as u32) as usize];
        let fault = match rng.below(7) {
            0 => Fault::LateAsn {
                shipment: s,
                delay: rng.range(10, 90),
            },
            1 => random_drift(&mut rng, scenario, s),
            2 => {
                let doc = random_doc(&mut rng, scenario, s);
                Fault::DuplicateEdi {
                    shipment: s,
                    doc,
                    after: rng.range(5, 30),
                }
            }
            3 => {
                let delayed: Vec<ShipmentId> = ids
                    .iter()
                    .copied()
                    .filter(|i| scenario.has_doc(*i, DocKind::Delayed))
                    .collect();
                if delayed.is_empty() {
                    Fault::CarrierNoShow { shipment: s }
                } else {
                    Fault::OutOfOrder {
                        shipment: delayed[rng.below(delayed.len() as u32) as usize],
                    }
                }
            }
            4 => Fault::TimeoutAfterCommit {
                tool: random_tool(&mut rng),
                nth: rng.below(ids.len() as u32),
            },
            5 => Fault::TimeoutBeforeCommit {
                tool: random_tool(&mut rng),
                nth: rng.below(ids.len() as u32),
            },
            _ => Fault::CarrierNoShow { shipment: s },
        };
        faults.push(fault);
    }
    faults.sort();
    faults.dedup();
    faults
}

fn random_tool(rng: &mut Rng) -> WriteKind {
    match rng.below(6) {
        0..=2 => WriteKind::PostReceipt,
        3 => WriteKind::BookDock,
        4 => WriteKind::SetPromise,
        _ => WriteKind::CancelDock,
    }
}

fn random_doc(rng: &mut Rng, scenario: &Scenario, s: ShipmentId) -> DocKind {
    let kinds: Vec<DocKind> = [
        DocKind::Asn,
        DocKind::InTransit,
        DocKind::Delayed,
        DocKind::Arrived,
    ]
    .into_iter()
    .filter(|k| scenario.has_doc(s, *k))
    .collect();
    kinds[rng.below(kinds.len() as u32) as usize]
}

fn random_drift(rng: &mut Rng, scenario: &Scenario, s: ShipmentId) -> Fault {
    let doc = random_doc(rng, scenario, s);
    let (field, renamed) = match doc {
        DocKind::Asn if rng.chance(50) => ("qty_shipped", "quantityShipped"),
        DocKind::Arrived => ("qty_received", "receivedQty"),
        _ => ("eta", "estimated_arrival"),
    };
    let drift = if rng.chance(50) {
        Drift::Renamed {
            to: renamed.to_string(),
        }
    } else {
        Drift::Missing
    };
    Fault::SchemaDrift {
        shipment: s,
        doc,
        field: field.to_string(),
        drift,
    }
}

/// One document scheduled for delivery.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub at: Minutes,
    pub seq: u32,
    pub doc: Document,
    /// Known to the oracle only. The agent never sees this.
    pub drifted: bool,
    pub notes: Vec<String>,
}

/// The delivery order plus the physical truth the oracle judges against.
#[derive(Debug, Clone)]
pub struct Feed {
    pub deliveries: Vec<Delivery>,
    /// Shipment to (arrival time, quantity physically delivered).
    pub truth: BTreeMap<ShipmentId, (Minutes, u32)>,
}

pub fn build_feed(scenario: &Scenario, faults: &[Fault]) -> Feed {
    let mut deliveries: Vec<Delivery> = scenario
        .documents
        .iter()
        .enumerate()
        .map(|(i, d)| Delivery {
            at: d.occurred_at,
            seq: i as u32,
            doc: d.clone(),
            drifted: false,
            notes: Vec::new(),
        })
        .collect();

    let mut ordered: Vec<&Fault> = faults.iter().collect();
    ordered.sort();
    for fault in &ordered {
        if let Fault::CarrierNoShow { shipment } = fault {
            apply_no_show(&mut deliveries, *shipment);
        }
    }

    // Truth is taken after no-shows move arrivals, and before drift can damage
    // the quantity field.
    let truth = deliveries
        .iter()
        .filter(|d| d.doc.kind == DocKind::Arrived)
        .map(|d| {
            let qty = d.doc.field("qty_received").unwrap_or(0);
            (d.doc.shipment, (d.doc.occurred_at, qty))
        })
        .collect();

    for fault in &ordered {
        match fault {
            Fault::OutOfOrder { shipment } => apply_out_of_order(&mut deliveries, *shipment),
            Fault::LateAsn { shipment, delay } => {
                apply_late_asn(&mut deliveries, *shipment, *delay)
            }
            Fault::SchemaDrift {
                shipment,
                doc,
                field,
                drift,
            } => apply_drift(&mut deliveries, *shipment, *doc, field, drift),
            Fault::DuplicateEdi {
                shipment,
                doc,
                after,
            } => apply_duplicate(&mut deliveries, *shipment, *doc, *after),
            _ => {}
        }
    }

    deliveries.sort_by_key(|d| (d.at, d.seq));
    Feed { deliveries, truth }
}

fn find(deliveries: &mut [Delivery], s: ShipmentId, kind: DocKind) -> Option<&mut Delivery> {
    deliveries
        .iter_mut()
        .find(|d| d.doc.shipment == s && d.doc.kind == kind)
}

fn next_seq(deliveries: &[Delivery]) -> u32 {
    deliveries.iter().map(|d| d.seq + 1).max().unwrap_or(0)
}

fn apply_no_show(deliveries: &mut Vec<Delivery>, s: ShipmentId) {
    if deliveries
        .iter()
        .any(|d| d.doc.shipment == s && d.doc.kind == DocKind::NoShow)
    {
        return;
    }
    let Some(arrived) = find(deliveries, s, DocKind::Arrived) else {
        return;
    };
    let missed = arrived.doc.occurred_at;
    let recovered = missed + NO_SHOW_RECOVERY;
    arrived.doc.occurred_at = recovered;
    arrived.at = recovered;

    let seq = next_seq(deliveries);
    let base = 900_000 + s.0 * 10;
    let mut no_show = Delivery {
        at: missed,
        seq,
        doc: make_doc(base, DocKind::NoShow, s, missed, &[]),
        drifted: false,
        notes: Vec::new(),
    };
    no_show
        .notes
        .push("carrier-no-show: truck missed its slot".to_string());
    let report = missed + NO_SHOW_REPORT;
    let recovery = Delivery {
        at: report,
        seq: seq + 1,
        doc: make_doc(base + 1, DocKind::Delayed, s, report, &[("eta", recovered)]),
        drifted: false,
        notes: Vec::new(),
    };
    deliveries.push(no_show);
    deliveries.push(recovery);
}

fn apply_out_of_order(deliveries: &mut [Delivery], s: ShipmentId) {
    let transit = deliveries
        .iter()
        .position(|d| d.doc.shipment == s && d.doc.kind == DocKind::InTransit);
    let delayed = deliveries
        .iter()
        .position(|d| d.doc.shipment == s && d.doc.kind == DocKind::Delayed);
    if let (Some(t), Some(d)) = (transit, delayed) {
        let (at_t, at_d) = (deliveries[t].at, deliveries[d].at);
        deliveries[t].at = at_d;
        deliveries[d].at = at_t;
        deliveries[t]
            .notes
            .push("out-of-order: older status delivered after newer one".to_string());
    }
}

fn apply_late_asn(deliveries: &mut [Delivery], s: ShipmentId, delay: Minutes) {
    let Some(arrival) = find(deliveries, s, DocKind::Arrived).map(|d| d.doc.occurred_at) else {
        return;
    };
    if let Some(asn) = find(deliveries, s, DocKind::Asn) {
        asn.at = arrival + delay;
        asn.notes.push(format!(
            "late-asn: delivered {delay}m after the truck arrived"
        ));
    }
}

fn apply_drift(
    deliveries: &mut [Delivery],
    s: ShipmentId,
    kind: DocKind,
    field: &str,
    drift: &Drift,
) {
    let Some(d) = find(deliveries, s, kind) else {
        return;
    };
    let Some(value) = d.doc.fields.remove(field) else {
        return;
    };
    d.drifted = true;
    match drift {
        Drift::Renamed { to } => {
            d.doc.fields.insert(to.clone(), value);
            d.notes
                .push(format!("schema-drift: field {field} renamed to {to}"));
        }
        Drift::Missing => d.notes.push(format!("schema-drift: field {field} missing")),
    }
}

fn apply_duplicate(deliveries: &mut Vec<Delivery>, s: ShipmentId, kind: DocKind, after: Minutes) {
    let seq = next_seq(deliveries);
    let Some(original) = find(deliveries, s, kind) else {
        return;
    };
    let mut copy = original.clone();
    copy.at += after;
    copy.seq = seq;
    copy.notes = vec![format!(
        "duplicate-edi: control {} redelivered",
        copy.doc.control
    )];
    deliveries.push(copy);
}
