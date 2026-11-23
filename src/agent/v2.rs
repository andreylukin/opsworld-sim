//! The fixed policy. It keeps what it has been told per shipment, then
//! converges the systems of record toward that state after every document.

use super::Agent;
use crate::model::{DocKind, Document, Minutes, ShipmentId, HANDLING};
use crate::tools::{ShipmentView, ToolCall, ToolError, ToolPort, ToolReply};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ATTEMPTS: u32 = 3;

#[derive(Default)]
struct Ship {
    asn_qty: Option<u32>,
    eta: Option<Minutes>,
    status_at: Minutes,
    arrived: Option<u32>,
    escalated: bool,
}

/// Why handling of a document ended early.
enum Stop {
    Halted,
    Escalated,
}

#[derive(Default)]
pub struct CarefulAgent {
    seen: BTreeSet<String>,
    ships: BTreeMap<ShipmentId, Ship>,
}

fn read(port: &mut dyn ToolPort, s: ShipmentId) -> Result<ShipmentView, Stop> {
    match port.call(ToolCall::ReadShipment(s)) {
        Ok(ToolReply::Shipment(view)) => Ok(view),
        _ => Err(Stop::Halted),
    }
}

/// Parses strictly. Any missing or malformed required field is a schema break.
fn validate(doc: &Document) -> Result<(), String> {
    let required: &[&str] = match doc.kind {
        DocKind::Asn => &["qty_shipped", "eta"],
        DocKind::InTransit | DocKind::Delayed => &["eta"],
        DocKind::Arrived => &["qty_received"],
        DocKind::NoShow => &[],
    };
    match required.iter().find(|f| doc.field(f).is_none()) {
        Some(field) => Err(format!(
            "schema break in {}: {field} unreadable",
            doc.control
        )),
        None => Ok(()),
    }
}

impl CarefulAgent {
    fn escalate(&mut self, port: &mut dyn ToolPort, s: ShipmentId, reason: String) -> Stop {
        self.ships.entry(s).or_default().escalated = true;
        match port.call(ToolCall::Escalate {
            shipment: s,
            reason,
        }) {
            Err(ToolError::Halted) => Stop::Halted,
            _ => Stop::Escalated,
        }
    }

    /// Issues a write. After a timeout, reads state first and retries only if
    /// the write did not land.
    fn write(
        &mut self,
        port: &mut dyn ToolPort,
        request: ToolCall,
        landed: impl Fn(&ShipmentView) -> bool,
    ) -> Result<(), Stop> {
        let s = request.shipment();
        for _ in 0..MAX_ATTEMPTS {
            match port.call(request.clone()) {
                Ok(_) => return Ok(()),
                Err(ToolError::Halted) => return Err(Stop::Halted),
                Err(ToolError::Rejected(reason)) => return Err(self.escalate(port, s, reason)),
                Err(ToolError::Timeout) => {
                    if landed(&read(port, s)?) {
                        return Ok(());
                    }
                }
            }
        }
        Err(self.escalate(port, s, format!("{request} did not land")))
    }

    fn set_promise(
        &mut self,
        port: &mut dyn ToolPort,
        s: ShipmentId,
        at: Option<Minutes>,
    ) -> Result<(), Stop> {
        self.write(port, ToolCall::SetPromise { shipment: s, at }, move |v| {
            v.promise == at
        })
    }

    fn cancel(&mut self, port: &mut dyn ToolPort, s: ShipmentId) -> Result<(), Stop> {
        self.write(port, ToolCall::CancelDock(s), |v| v.appointment.is_none())
    }

    /// Moves ERP and TMS toward what is currently known about the shipment.
    fn converge(&mut self, port: &mut dyn ToolPort, s: ShipmentId) -> Result<(), Stop> {
        let ship = &self.ships[&s];
        // Buffer everything until the ASN gives us the expected quantity.
        let Some(asn_qty) = ship.asn_qty else {
            return Ok(());
        };
        let (eta, arrived) = (ship.eta, ship.arrived);
        let view = read(port, s)?;
        let short = asn_qty < view.ordered;
        if short && !view.flagged {
            if let Err(ToolError::Halted) = port.call(ToolCall::FlagMismatch(s)) {
                return Err(Stop::Halted);
            }
        }

        if let Some(qty) = arrived {
            if view.receipts == 0 {
                let post = ToolCall::PostReceipt { shipment: s, qty };
                self.write(port, post, |v| v.receipts > 0)?;
            }
            if !short && view.promise.is_none() {
                let at = Some(port.now() + HANDLING);
                self.set_promise(port, s, at)?;
            }
            return Ok(());
        }

        let Some(eta) = eta else {
            // No-show: nothing is known, so hold no door and promise nothing.
            if view.appointment.is_some() {
                self.cancel(port, s)?;
            }
            if view.promise.is_some() {
                self.set_promise(port, s, None)?;
            }
            return Ok(());
        };

        let start = match view.appointment {
            Some(start) if start >= eta => start,
            current => {
                if current.is_some() {
                    self.cancel(port, s)?;
                }
                let earliest = eta.max(port.now());
                let slot = port.call(ToolCall::FindSlot {
                    shipment: s,
                    earliest,
                });
                let Ok(ToolReply::Slot { door, start }) = slot else {
                    return Err(Stop::Halted);
                };
                let book = ToolCall::BookDock {
                    shipment: s,
                    door,
                    start,
                };
                self.write(port, book, |v| v.appointment.is_some())?;
                // A timed-out booking that landed is still the booking we asked for.
                read(port, s)?.appointment.unwrap_or(start)
            }
        };
        let target = (!short).then_some(start + HANDLING);
        if view.promise != target {
            self.set_promise(port, s, target)?;
        }
        Ok(())
    }

    fn process(&mut self, doc: &Document, port: &mut dyn ToolPort) -> Result<(), Stop> {
        if !self.seen.insert(doc.control.clone()) {
            return Ok(());
        }
        let s = doc.shipment;
        if self.ships.entry(s).or_default().escalated {
            return Ok(());
        }
        if let Err(reason) = validate(doc) {
            return Err(self.escalate(port, s, reason));
        }

        let ship = self.ships.get_mut(&s).expect("inserted above");
        let newest = doc.occurred_at >= ship.status_at;
        match doc.kind {
            DocKind::Asn => ship.asn_qty = doc.field("qty_shipped"),
            DocKind::Arrived => ship.arrived = doc.field("qty_received"),
            _ => {}
        }
        if newest {
            ship.status_at = doc.occurred_at;
            ship.eta = match doc.kind {
                DocKind::NoShow => None,
                DocKind::Arrived => Some(doc.occurred_at),
                _ => doc.field("eta"),
            };
        }
        self.converge(port, s)
    }
}

impl Agent for CarefulAgent {
    fn name(&self) -> &'static str {
        "v2"
    }

    fn handle(&mut self, doc: &Document, port: &mut dyn ToolPort) {
        // Both stop reasons are already recorded: in the trace, or as an escalation.
        let _ = self.process(doc, port);
    }
}
