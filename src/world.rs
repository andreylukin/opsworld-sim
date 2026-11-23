//! In-memory stand-ins for an ERP and a TMS. Like many real systems of record,
//! they accept what they are told: guarding against bad writes is not their job
//! here, so agent mistakes reach state and the oracle can observe them.

use crate::model::{Minutes, ShipmentId, SLOT};
use crate::tools::{ShipmentView, ToolCall, ToolReply};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct PurchaseOrder {
    pub sku: u32,
    pub ordered: u32,
    pub receipts: Vec<u32>,
    pub flagged: bool,
}

#[derive(Debug, Clone)]
pub struct OrderLine {
    pub sku: u32,
    pub qty: u32,
    pub promise: Option<Minutes>,
}

#[derive(Debug, Clone, Default)]
pub struct Erp {
    pub pos: BTreeMap<ShipmentId, PurchaseOrder>,
    pub lines: BTreeMap<ShipmentId, OrderLine>,
    pub inventory: BTreeMap<u32, u32>,
    pub initial: BTreeMap<u32, u32>,
}

#[derive(Debug, Clone)]
pub struct Appointment {
    pub shipment: ShipmentId,
    pub door: u32,
    pub truck: u32,
    pub start: Minutes,
    pub active: bool,
}

impl Appointment {
    pub fn overlaps(&self, start: Minutes) -> bool {
        self.start < start + SLOT && start < self.start + SLOT
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tms {
    pub doors: u32,
    pub carriers: BTreeMap<ShipmentId, String>,
    pub trucks: BTreeMap<ShipmentId, u32>,
    pub appointments: Vec<Appointment>,
}

impl Tms {
    fn active(&self, shipment: ShipmentId) -> Option<&Appointment> {
        self.appointments
            .iter()
            .find(|a| a.active && a.shipment == shipment)
    }

    fn find_slot(&self, earliest: Minutes) -> (u32, Minutes) {
        let mut start = earliest;
        loop {
            for door in 1..=self.doors {
                let taken = self
                    .appointments
                    .iter()
                    .any(|a| a.active && a.door == door && a.overlaps(start));
                if !taken {
                    return (door, start);
                }
            }
            start += 15;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct World {
    pub erp: Erp,
    pub tms: Tms,
    pub escalated: BTreeSet<ShipmentId>,
}

impl World {
    /// Executes one call against state. Returns the reply and a short note on
    /// what changed, for the trace.
    pub fn apply(&mut self, call: &ToolCall) -> Result<(ToolReply, String), String> {
        let s = call.shipment();
        if !self.erp.pos.contains_key(&s) {
            return Err(format!("unknown shipment {s}"));
        }
        match call {
            ToolCall::ReadPo(_) => {
                let ordered = self.erp.pos[&s].ordered;
                Ok((ToolReply::Po { ordered }, format!("ordered={ordered}")))
            }
            ToolCall::ReadShipment(_) => {
                let po = &self.erp.pos[&s];
                let view = ShipmentView {
                    ordered: po.ordered,
                    receipts: po.receipts.len(),
                    appointment: self.tms.active(s).map(|a| a.start),
                    promise: self.erp.lines[&s].promise,
                    flagged: po.flagged,
                };
                let note = format!(
                    "receipts={} appointment={} promise={}",
                    view.receipts,
                    if view.appointment.is_some() {
                        "yes"
                    } else {
                        "no"
                    },
                    if view.promise.is_some() { "yes" } else { "no" },
                );
                Ok((ToolReply::Shipment(view), note))
            }
            ToolCall::FindSlot { earliest, .. } => {
                let (door, start) = self.tms.find_slot(*earliest);
                Ok((
                    ToolReply::Slot { door, start },
                    format!("door={door} start={}", crate::model::clock(start)),
                ))
            }
            ToolCall::BookDock { door, start, .. } => {
                let truck = self.tms.trucks[&s];
                self.tms.appointments.push(Appointment {
                    shipment: s,
                    door: *door,
                    truck,
                    start: *start,
                    active: true,
                });
                Ok((ToolReply::Done, format!("booked truck T-{truck}")))
            }
            ToolCall::CancelDock(_) => {
                let found = self
                    .tms
                    .appointments
                    .iter_mut()
                    .find(|a| a.active && a.shipment == s);
                match found {
                    Some(a) => {
                        a.active = false;
                        Ok((ToolReply::Done, "released".to_string()))
                    }
                    None => Err("no active appointment".to_string()),
                }
            }
            ToolCall::SetPromise { at, .. } => {
                self.erp.lines.get_mut(&s).expect("checked above").promise = *at;
                Ok((ToolReply::Done, "updated".to_string()))
            }
            ToolCall::PostReceipt { qty, .. } => {
                let po = self.erp.pos.get_mut(&s).expect("checked above");
                po.receipts.push(*qty);
                let stock = self.erp.inventory.entry(po.sku).or_insert(0);
                let before = *stock;
                *stock += qty;
                Ok((ToolReply::Done, format!("on_hand {before} -> {stock}")))
            }
            ToolCall::FlagMismatch(_) => {
                self.erp.pos.get_mut(&s).expect("checked above").flagged = true;
                Ok((ToolReply::Done, "flagged".to_string()))
            }
            ToolCall::Escalate { .. } => {
                self.escalated.insert(s);
                Ok((ToolReply::Done, "handed to a human".to_string()))
            }
        }
    }
}
