//! The oracle. It sees ground truth and judges the agent only on information
//! that has already been delivered to it.

use crate::faults::Delivery;
use crate::model::{clock, DocKind, Minutes, ShipmentId, HANDLING};
use crate::tools::{ToolCall, WriteKind};
use crate::world::World;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Invariant {
    NoBlindRetry,
    NoDoubleApply,
    NoWriteAfterSchemaBreak,
    NoDoubleBook,
    PromiseFeasible,
    QuantitiesReconcile,
}

impl Invariant {
    pub const ALL: [Self; 6] = [
        Self::NoBlindRetry,
        Self::NoDoubleApply,
        Self::NoWriteAfterSchemaBreak,
        Self::NoDoubleBook,
        Self::PromiseFeasible,
        Self::QuantitiesReconcile,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::NoBlindRetry => "no-blind-retry",
            Self::NoDoubleApply => "no-double-apply",
            Self::NoWriteAfterSchemaBreak => "no-write-after-schema-break",
            Self::NoDoubleBook => "no-double-book",
            Self::PromiseFeasible => "promise-feasible",
            Self::QuantitiesReconcile => "quantities-reconcile",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|i| i.id() == id)
    }
}

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub invariant: Invariant,
    pub shipment: Option<ShipmentId>,
    pub detail: String,
}

/// What has been reported to the agent about one shipment.
#[derive(Debug, Clone, Default)]
struct Known {
    asn_qty: Option<u32>,
    arrived_qty: Option<u32>,
    eta: Option<Minutes>,
    latest: Minutes,
}

#[derive(Debug, Default)]
pub struct Oracle {
    truth: BTreeMap<ShipmentId, (Minutes, u32)>,
    known: BTreeMap<ShipmentId, Known>,
    unknown: BTreeSet<(WriteKind, ShipmentId)>,
    broken: BTreeSet<ShipmentId>,
}

impl Oracle {
    pub fn new(truth: BTreeMap<ShipmentId, (Minutes, u32)>) -> Self {
        Self {
            truth,
            ..Self::default()
        }
    }

    /// Records what a delivered document told the agent. Newest event time
    /// wins, so a stale update cannot make an old promise look feasible.
    pub fn observe(&mut self, delivery: &Delivery) {
        let doc = &delivery.doc;
        if delivery.drifted {
            self.broken.insert(doc.shipment);
            return;
        }
        let known = self.known.entry(doc.shipment).or_default();
        let newest = doc.occurred_at >= known.latest;
        match doc.kind {
            DocKind::Asn => known.asn_qty = doc.field("qty_shipped"),
            DocKind::Arrived => known.arrived_qty = doc.field("qty_received"),
            _ => {}
        }
        if newest {
            known.latest = doc.occurred_at;
            known.eta = match doc.kind {
                DocKind::NoShow => None,
                DocKind::Arrived => Some(doc.occurred_at),
                _ => doc.field("eta"),
            };
        }
    }

    /// Checks made before a call executes, against what the agent knew.
    pub fn before_call(&mut self, call: &ToolCall) -> Vec<Violation> {
        let s = call.shipment();
        let mut found = Vec::new();
        if let ToolCall::ReadShipment(_) = call {
            self.unknown.retain(|(_, shipment)| *shipment != s);
        }
        if let Some(kind) = call.write_kind() {
            if self.unknown.contains(&(kind, s)) {
                found.push(Violation {
                    invariant: Invariant::NoBlindRetry,
                    shipment: Some(s),
                    detail: format!(
                        "{kind} for {s} reissued after a timeout, with no read of {s} in between"
                    ),
                });
            }
        }
        if call.is_business_write() && self.broken.contains(&s) {
            found.push(Violation {
                invariant: Invariant::NoWriteAfterSchemaBreak,
                shipment: Some(s),
                detail: format!("wrote to {s} after receiving a malformed document for it"),
            });
        }
        found
    }

    pub fn record_outcome(&mut self, call: &ToolCall, timed_out: bool) {
        if let Some(kind) = call.write_kind() {
            let key = (kind, call.shipment());
            if timed_out {
                self.unknown.insert(key);
            } else {
                self.unknown.remove(&key);
            }
        }
    }

    /// Checks made after a write has reached state.
    pub fn after_write(&self, world: &World) -> Vec<Violation> {
        let mut found = Vec::new();
        for (s, po) in &world.erp.pos {
            if po.receipts.len() > 1 {
                let parts: Vec<String> = po.receipts.iter().map(u32::to_string).collect();
                let physical = self.truth.get(s).map_or(0, |t| t.1);
                found.push(Violation {
                    invariant: Invariant::NoDoubleApply,
                    shipment: Some(*s),
                    detail: format!(
                        "{s} receipt applied {} times ({}), physically delivered {physical}, on_hand now {}",
                        po.receipts.len(),
                        parts.join(" + "),
                        world.erp.inventory[&po.sku],
                    ),
                });
            }
        }
        let active: Vec<_> = world.tms.appointments.iter().filter(|a| a.active).collect();
        for (i, a) in active.iter().enumerate() {
            for b in &active[i + 1..] {
                let what = if a.shipment == b.shipment {
                    Some(format!(
                        "order line for {} has two active appointments",
                        a.shipment
                    ))
                } else if a.door == b.door && a.overlaps(b.start) {
                    Some(format!(
                        "door {} booked for {} and {}",
                        a.door, a.shipment, b.shipment
                    ))
                } else if a.truck == b.truck && a.overlaps(b.start) {
                    Some(format!("truck T-{} booked twice", a.truck))
                } else {
                    None
                };
                if let Some(what) = what {
                    found.push(Violation {
                        invariant: Invariant::NoDoubleBook,
                        shipment: Some(b.shipment),
                        detail: format!("{what} (at {} and {})", clock(a.start), clock(b.start)),
                    });
                }
            }
        }
        found
    }

    /// Checks made once the agent has finished handling a document.
    pub fn after_document(&self, world: &World, now: Minutes) -> Vec<Violation> {
        let mut found = Vec::new();
        for (s, line) in &world.erp.lines {
            let exempt = self.broken.contains(s) || world.escalated.contains(s);
            if let (Some(promise), false) = (line.promise, exempt) {
                let stock = world.erp.inventory[&line.sku];
                let inbound = self.inbound_by(world, *s, promise);
                if stock + inbound < line.qty {
                    found.push(Violation {
                        invariant: Invariant::PromiseFeasible,
                        shipment: Some(*s),
                        detail: format!(
                            "{s} promised {} for {} units, but stock {stock} + transit {inbound} by then is short",
                            clock(promise),
                            line.qty,
                        ),
                    });
                }
            }
        }
        for (s, po) in &world.erp.pos {
            let received: u32 = po.receipts.iter().sum();
            let expected = world.erp.initial[&po.sku] + received;
            let on_hand = world.erp.inventory[&po.sku];
            let physical = match self.truth.get(s) {
                Some((at, qty)) if *at <= now => *qty,
                _ => 0,
            };
            if on_hand != expected || received > physical {
                found.push(Violation {
                    invariant: Invariant::QuantitiesReconcile,
                    shipment: Some(*s),
                    detail: format!(
                        "{s} books show {received} received, {physical} physically delivered, on_hand {on_hand}"
                    ),
                });
            }
        }
        found
    }

    /// End of run: everything that physically arrived is on the books, unless
    /// the shipment was withheld because of a schema break or handed to a human.
    pub fn at_end(&self, world: &World) -> Vec<Violation> {
        let mut found = Vec::new();
        for (s, (_, qty)) in &self.truth {
            if self.broken.contains(s) || world.escalated.contains(s) {
                continue;
            }
            let received: u32 = world.erp.pos[s].receipts.iter().sum();
            if received != *qty {
                found.push(Violation {
                    invariant: Invariant::QuantitiesReconcile,
                    shipment: Some(*s),
                    detail: format!("{s} physically delivered {qty}, but books show {received}"),
                });
            }
        }
        found
    }

    fn inbound_by(&self, world: &World, s: ShipmentId, promise: Minutes) -> u32 {
        if !world.erp.pos[&s].receipts.is_empty() {
            return 0;
        }
        let Some(known) = self.known.get(&s) else {
            return 0;
        };
        let qty = known.arrived_qty.or(known.asn_qty).unwrap_or(0);
        match known.eta {
            Some(eta) if eta + HANDLING <= promise => qty,
            _ => 0,
        }
    }
}
