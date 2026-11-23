//! Shared domain types: virtual time, identifiers and inbound documents.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Virtual minutes since the start of the scenario.
pub type Minutes = u32;

/// Time from truck arrival to goods being available.
pub const HANDLING: Minutes = 120;
/// Length of a dock appointment.
pub const SLOT: Minutes = 60;

pub fn clock(t: Minutes) -> String {
    format!("D{} {:02}:{:02}", t / 1440 + 1, (t % 1440) / 60, t % 60)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ShipmentId(pub u32);

impl fmt::Display for ShipmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SH-{:02}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocKind {
    Asn,
    InTransit,
    Delayed,
    NoShow,
    Arrived,
}

impl fmt::Display for DocKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Asn => "ASN",
            Self::InTransit => "STATUS in_transit",
            Self::Delayed => "STATUS delayed",
            Self::NoShow => "STATUS no_show",
            Self::Arrived => "STATUS arrived",
        })
    }
}

/// An EDI-like document. Fields are untyped strings, so a renamed or missing
/// field is a real parsing problem for the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub control: String,
    pub kind: DocKind,
    pub shipment: ShipmentId,
    pub occurred_at: Minutes,
    pub fields: BTreeMap<String, String>,
}

impl Document {
    pub fn field(&self, name: &str) -> Option<u32> {
        self.fields.get(name).and_then(|v| v.parse().ok())
    }

    pub fn summary(&self) -> String {
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| match (k.as_str(), v.parse()) {
                ("eta" | "requested_dock_appt", Ok(t)) => format!("{k}={}", clock(t)),
                _ => format!("{k}={v}"),
            })
            .collect();
        format!(
            "{} {} {} {}",
            self.kind,
            self.control,
            self.shipment,
            fields.join(" ")
        )
        .trim_end()
        .to_string()
    }
}
