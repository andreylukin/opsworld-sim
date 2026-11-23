//! A reasonable first pass at an ops agent. It tracks each shipment, handles
//! no-shows, keeps doors in step with the latest ETA, and hands errors it cannot
//! resolve to a person. Its remaining assumptions are ordinary ones: a timed-out
//! write probably failed, documents arrive once and in order, and a missing
//! field can be filled from what is already known.

use super::Agent;
use crate::model::{DocKind, Document, Minutes, ShipmentId, HANDLING};
use crate::tools::{ToolCall, ToolError, ToolPort, ToolReply, ToolResult};
use std::collections::BTreeMap;

const MAX_ATTEMPTS: u32 = 3;
const DEFAULT_LEAD: Minutes = 1440;

#[derive(Default)]
struct Ship {
    /// Quantity from the ASN, once one has been seen.
    qty: Option<u32>,
    eta: Option<Minutes>,
    short: bool,
    booked: bool,
    promised: bool,
    received: bool,
    escalated: bool,
}

#[derive(Default)]
pub struct FirstPassAgent {
    ships: BTreeMap<ShipmentId, Ship>,
}

/// Retries a timeout a bounded number of times.
fn call(port: &mut dyn ToolPort, request: ToolCall) -> ToolResult {
    let mut result = Err(ToolError::Timeout);
    for _ in 0..MAX_ATTEMPTS {
        result = port.call(request.clone());
        if !matches!(result, Err(ToolError::Timeout)) {
            break;
        }
    }
    result
}

fn book(port: &mut dyn ToolPort, s: ShipmentId, ship: &mut Ship) -> Result<(), ToolError> {
    let Some(eta) = ship.eta else {
        return Ok(());
    };
    let earliest = eta.max(port.now());
    let slot = call(
        port,
        ToolCall::FindSlot {
            shipment: s,
            earliest,
        },
    )?;
    let ToolReply::Slot { door, start } = slot else {
        return Ok(());
    };
    call(
        port,
        ToolCall::BookDock {
            shipment: s,
            door,
            start,
        },
    )?;
    ship.booked = true;
    if !ship.short {
        let at = Some(start + HANDLING);
        call(port, ToolCall::SetPromise { shipment: s, at })?;
        ship.promised = true;
    }
    Ok(())
}

fn release(port: &mut dyn ToolPort, s: ShipmentId, ship: &mut Ship) -> Result<(), ToolError> {
    if ship.booked {
        call(port, ToolCall::CancelDock(s))?;
        ship.booked = false;
    }
    Ok(())
}

fn on_asn(port: &mut dyn ToolPort, doc: &Document, ship: &mut Ship) -> Result<(), ToolError> {
    let s = doc.shipment;
    let ToolReply::Po { ordered } = call(port, ToolCall::ReadPo(s))? else {
        return Ok(());
    };
    let qty = doc.field("qty_shipped").unwrap_or(ordered);
    ship.qty = Some(qty);
    ship.short = qty < ordered;
    if ship.short {
        call(port, ToolCall::FlagMismatch(s))?;
    }
    if ship.received {
        return Ok(());
    }
    ship.eta = Some(doc.field("eta").unwrap_or(doc.occurred_at + DEFAULT_LEAD));
    book(port, s, ship)
}

fn on_status(port: &mut dyn ToolPort, doc: &Document, ship: &mut Ship) -> Result<(), ToolError> {
    // Without an ASN there is no quantity to plan around, so wait for it.
    if ship.qty.is_none() || ship.received {
        return Ok(());
    }
    let eta = doc.field("eta").or(ship.eta);
    if eta != ship.eta || !ship.booked {
        ship.eta = eta;
        release(port, doc.shipment, ship)?;
        book(port, doc.shipment, ship)?;
    }
    Ok(())
}

fn on_no_show(port: &mut dyn ToolPort, s: ShipmentId, ship: &mut Ship) -> Result<(), ToolError> {
    ship.eta = None;
    release(port, s, ship)?;
    if ship.promised {
        call(
            port,
            ToolCall::SetPromise {
                shipment: s,
                at: None,
            },
        )?;
        ship.promised = false;
    }
    Ok(())
}

fn on_arrived(port: &mut dyn ToolPort, doc: &Document, ship: &mut Ship) -> Result<(), ToolError> {
    let s = doc.shipment;
    let Some(qty) = doc.field("qty_received").or(ship.qty) else {
        return Err(ToolError::Rejected(format!(
            "{} has no readable quantity and no ASN",
            doc.control
        )));
    };
    call(port, ToolCall::PostReceipt { shipment: s, qty })?;
    ship.received = true;
    Ok(())
}

impl Agent for FirstPassAgent {
    fn name(&self) -> &'static str {
        "v1"
    }

    fn handle(&mut self, doc: &Document, port: &mut dyn ToolPort) {
        let s = doc.shipment;
        let ship = self.ships.entry(s).or_default();
        if ship.escalated {
            return;
        }
        let outcome = match doc.kind {
            DocKind::Asn => on_asn(port, doc, ship),
            DocKind::InTransit | DocKind::Delayed => on_status(port, doc, ship),
            DocKind::NoShow => on_no_show(port, s, ship),
            DocKind::Arrived => on_arrived(port, doc, ship),
        };
        let reason = match outcome {
            Ok(()) | Err(ToolError::Halted) => return,
            Err(ToolError::Timeout) => format!("{} still timing out after retries", doc.control),
            Err(ToolError::Rejected(reason)) => reason,
        };
        ship.escalated = true;
        let _ = port.call(ToolCall::Escalate {
            shipment: s,
            reason,
        });
    }
}
