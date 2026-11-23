//! The complete side-effect surface available to an agent.

use crate::model::{clock, Minutes, ShipmentId};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteKind {
    BookDock,
    CancelDock,
    SetPromise,
    PostReceipt,
}

impl fmt::Display for WriteKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BookDock => "tms.book_dock",
            Self::CancelDock => "tms.cancel_dock",
            Self::SetPromise => "erp.set_promise",
            Self::PostReceipt => "erp.post_receipt",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    ReadPo(ShipmentId),
    ReadShipment(ShipmentId),
    FindSlot {
        shipment: ShipmentId,
        earliest: Minutes,
    },
    BookDock {
        shipment: ShipmentId,
        door: u32,
        start: Minutes,
    },
    CancelDock(ShipmentId),
    /// `None` retracts the confirmed promise.
    SetPromise {
        shipment: ShipmentId,
        at: Option<Minutes>,
    },
    PostReceipt {
        shipment: ShipmentId,
        qty: u32,
    },
    FlagMismatch(ShipmentId),
    Escalate {
        shipment: ShipmentId,
        reason: String,
    },
}

impl ToolCall {
    pub fn shipment(&self) -> ShipmentId {
        match self {
            Self::ReadPo(s)
            | Self::ReadShipment(s)
            | Self::CancelDock(s)
            | Self::FlagMismatch(s) => *s,
            Self::FindSlot { shipment, .. }
            | Self::BookDock { shipment, .. }
            | Self::SetPromise { shipment, .. }
            | Self::PostReceipt { shipment, .. }
            | Self::Escalate { shipment, .. } => *shipment,
        }
    }

    /// The fault-injectable write kind, if this call is one.
    pub fn write_kind(&self) -> Option<WriteKind> {
        match self {
            Self::BookDock { .. } => Some(WriteKind::BookDock),
            Self::CancelDock(_) => Some(WriteKind::CancelDock),
            Self::SetPromise { .. } => Some(WriteKind::SetPromise),
            Self::PostReceipt { .. } => Some(WriteKind::PostReceipt),
            _ => None,
        }
    }

    /// True for calls that change a system of record on the agent's judgment.
    /// Escalation is excluded because it hands the decision to a person.
    pub fn is_business_write(&self) -> bool {
        self.write_kind().is_some() || matches!(self, Self::FlagMismatch(_))
    }

    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Self::ReadPo(_) | Self::ReadShipment(_) | Self::FindSlot { .. }
        )
    }
}

impl fmt::Display for ToolCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadPo(s) => write!(f, "erp.read_po {s}"),
            Self::ReadShipment(s) => write!(f, "erp.read_shipment {s}"),
            Self::FindSlot { shipment, earliest } => {
                write!(f, "tms.find_slot {shipment} earliest={}", clock(*earliest))
            }
            Self::BookDock {
                shipment,
                door,
                start,
            } => write!(
                f,
                "tms.book_dock {shipment} door={door} start={}",
                clock(*start)
            ),
            Self::CancelDock(s) => write!(f, "tms.cancel_dock {s}"),
            Self::SetPromise {
                shipment,
                at: Some(t),
            } => write!(f, "erp.set_promise {shipment} confirm={}", clock(*t)),
            Self::SetPromise { shipment, at: None } => {
                write!(f, "erp.set_promise {shipment} retract")
            }
            Self::PostReceipt { shipment, qty } => {
                write!(f, "erp.post_receipt {shipment} qty={qty}")
            }
            Self::FlagMismatch(s) => write!(f, "erp.flag_mismatch {s}"),
            Self::Escalate { shipment, reason } => write!(f, "ops.escalate {shipment} ({reason})"),
        }
    }
}

/// What an agent can learn about one shipment by reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipmentView {
    pub ordered: u32,
    pub receipts: usize,
    pub appointment: Option<Minutes>,
    pub promise: Option<Minutes>,
    pub flagged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolReply {
    Po { ordered: u32 },
    Shipment(ShipmentView),
    Slot { door: u32, start: Minutes },
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The outcome is unknown: the write may or may not have committed.
    Timeout,
    Rejected(String),
    /// The simulation stopped because an invariant fired.
    Halted,
}

pub type ToolResult = Result<ToolReply, ToolError>;

/// The agent's only route to the world.
pub trait ToolPort {
    fn now(&self) -> Minutes;
    fn call(&mut self, call: ToolCall) -> ToolResult;
}
