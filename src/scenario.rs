//! Generates a synthetic world and its baseline document feed from a seed.

use crate::model::{DocKind, Document, Minutes, ShipmentId};
use crate::rng::Rng;
use crate::world::{OrderLine, PurchaseOrder, World};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

const WORLD_STREAM: u64 = 1;

/// The named document shapes a run can use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum ScenarioKind {
    #[default]
    #[serde(rename = "baseline")]
    Baseline,
    /// EDI 856 ASNs flowing into receiving and a promise-date update
    #[serde(rename = "asn-856")]
    #[value(name = "asn-856")]
    Asn856,
}

impl fmt::Display for ScenarioKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Baseline => "baseline",
            Self::Asn856 => "asn-856",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub kind: ScenarioKind,
    pub seed: u64,
    pub world: World,
    pub documents: Vec<Document>,
}

pub fn make_doc(
    control: u32,
    kind: DocKind,
    shipment: ShipmentId,
    occurred_at: Minutes,
    fields: &[(&str, u32)],
) -> Document {
    Document {
        control: format!("EDI{control:06}"),
        kind,
        shipment,
        occurred_at,
        fields: fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// Adds the envelope and reference fields of an EDI 856 (or 214 for carrier
/// status). SCAC codes are placeholders and do not identify a real carrier.
fn dress_as_856(doc: &mut Document, carrier: &str) {
    let n = doc.shipment.0;
    let scac = format!("ZZZ{}", carrier.chars().last().unwrap_or('A'));
    let set = if doc.kind == DocKind::Asn {
        "856"
    } else {
        "214"
    };
    for (key, value) in [
        ("transaction_set", set.to_string()),
        ("asn", format!("ASN-{}", 70_000 + n)),
        ("po", format!("PO-{}", 1000 + n)),
        ("line", "1".to_string()),
        ("scac", scac),
    ] {
        doc.fields.insert(key.to_string(), value);
    }
    if doc.kind == DocKind::Asn {
        if let Some(eta) = doc.fields.get("eta").cloned() {
            doc.fields.insert("requested_dock_appt".to_string(), eta);
        }
    }
}

impl Scenario {
    /// Same world, timing and fault schedule as the baseline for this seed.
    pub fn build(kind: ScenarioKind, seed: u64) -> Self {
        let mut scenario = Self::generate(seed);
        scenario.kind = kind;
        if kind == ScenarioKind::Asn856 {
            for doc in &mut scenario.documents {
                let carrier = scenario.world.tms.carriers[&doc.shipment].clone();
                dress_as_856(doc, &carrier);
            }
        }
        scenario
    }

    pub fn generate(seed: u64) -> Self {
        let mut rng = Rng::fork(seed, WORLD_STREAM);
        let mut world = World::default();
        world.tms.doors = rng.range(2, 3);
        let mut documents = Vec::new();
        let mut control = 1;
        let mut doc = |kind, s, at, fields: &[(&str, u32)]| {
            let d = make_doc(control, kind, s, at, fields);
            control += 1;
            d
        };

        for i in 1..=rng.range(3, 6) {
            let s = ShipmentId(i);
            let sku = 100 + i;
            let ordered = rng.range(4, 20) * 10;
            let stock = rng.range(1, 5) * 10;
            let shipped = if rng.chance(20) {
                ordered - rng.range(1, 3) * 10
            } else {
                ordered
            };

            world.erp.pos.insert(
                s,
                PurchaseOrder {
                    sku,
                    ordered,
                    receipts: Vec::new(),
                    flagged: false,
                },
            );
            world.erp.lines.insert(
                s,
                OrderLine {
                    sku,
                    qty: stock + ordered,
                    promise: None,
                },
            );
            world.erp.inventory.insert(sku, stock);
            world.erp.initial.insert(sku, stock);
            world.tms.carriers.insert(
                s,
                format!("CARRIER-{}", (b'A' + rng.below(4) as u8) as char),
            );
            world.tms.trucks.insert(s, 100 + i);

            let asn_at = rng.range(0, 240);
            let transit_at = asn_at + rng.range(30, 120);
            let planned = transit_at + rng.range(240, 720);
            documents.push(doc(
                DocKind::Asn,
                s,
                asn_at,
                &[("po", 1000 + i), ("qty_shipped", shipped), ("eta", planned)],
            ));
            documents.push(doc(DocKind::InTransit, s, transit_at, &[("eta", planned)]));
            let mut eta = planned;
            if rng.chance(50) {
                let delayed_at = rng.range(transit_at + 15, planned - 15);
                eta = planned + rng.range(60, 360);
                documents.push(doc(DocKind::Delayed, s, delayed_at, &[("eta", eta)]));
            }
            documents.push(doc(DocKind::Arrived, s, eta, &[("qty_received", shipped)]));
        }

        Self {
            kind: ScenarioKind::Baseline,
            seed,
            world,
            documents,
        }
    }

    pub fn shipment_ids(&self) -> Vec<ShipmentId> {
        self.world.erp.pos.keys().copied().collect()
    }

    pub fn has_doc(&self, shipment: ShipmentId, kind: DocKind) -> bool {
        self.documents
            .iter()
            .any(|d| d.shipment == shipment && d.kind == kind)
    }
}
