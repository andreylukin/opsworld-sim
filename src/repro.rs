//! Repro files and schedule shrinking.

use crate::agent::llm::Transcript;
use crate::agent::Policy;
use crate::faults::Fault;
use crate::invariants::Invariant;
use crate::model::ShipmentId;
use crate::scenario::ScenarioKind;
use crate::sim::{run_in, RunResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repro {
    #[serde(default)]
    pub scenario: ScenarioKind,
    pub seed: u64,
    pub policy: Policy,
    /// `None` for a recorded LLM run that held every invariant.
    pub invariant: Option<Invariant>,
    pub shipment: Option<ShipmentId>,
    pub step: u32,
    pub trace_hash: String,
    /// The full schedule, before shrinking.
    #[serde(default)]
    pub faults_before: Vec<Fault>,
    pub faults: Vec<Fault>,
    /// Every model request and response, for LLM runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<Transcript>,
}

fn fires(
    kind: ScenarioKind,
    seed: u64,
    faults: &[Fault],
    policy: Policy,
    target: Invariant,
) -> bool {
    run_in(kind, seed, faults, policy)
        .violations
        .iter()
        .any(|v| v.invariant == target)
}

/// Greedy reduction: drops any fault whose removal still fires `target`,
/// until no single fault can be removed.
pub fn shrink(seed: u64, faults: &[Fault], policy: Policy, target: Invariant) -> Vec<Fault> {
    shrink_in(ScenarioKind::Baseline, seed, faults, policy, target)
}

/// `shrink` for a named scenario.
pub fn shrink_in(
    kind: ScenarioKind,
    seed: u64,
    faults: &[Fault],
    policy: Policy,
    target: Invariant,
) -> Vec<Fault> {
    let mut current = faults.to_vec();
    let mut i = 0;
    while i < current.len() {
        let mut candidate = current.clone();
        candidate.remove(i);
        if fires(kind, seed, &candidate, policy, target) {
            current = candidate;
            i = 0;
        } else {
            i += 1;
        }
    }
    current
}

impl Repro {
    /// Shrinks a failing run and captures the result.
    pub fn from_failure(result: &RunResult, policy: Policy, target: Invariant) -> Self {
        let kind = result.scenario;
        let faults = shrink_in(kind, result.seed, &result.faults, policy, target);
        let minimal = run_in(kind, result.seed, &faults, policy);
        let shipment = minimal
            .violations
            .iter()
            .find(|v| v.invariant == target)
            .and_then(|v| v.shipment);
        Self {
            scenario: kind,
            seed: result.seed,
            policy,
            invariant: Some(target),
            shipment,
            step: minimal.violation_step.unwrap_or(0),
            trace_hash: minimal.hash,
            faults_before: result.faults.clone(),
            faults,
            transcript: None,
        }
    }

    /// Captures a recorded LLM run. Its schedule is not shrunk, because each
    /// shrink step would need new live model calls.
    pub fn from_llm(result: &RunResult, transcript: Transcript) -> Self {
        let first = result.violations.first();
        Self {
            scenario: result.scenario,
            seed: result.seed,
            policy: Policy::Llm,
            invariant: first.map(|v| v.invariant),
            shipment: first.and_then(|v| v.shipment),
            step: result.violation_step.unwrap_or(0),
            trace_hash: result.hash.clone(),
            faults_before: result.faults.clone(),
            faults: result.faults.clone(),
            transcript: Some(transcript),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let prefix = match self.scenario {
            ScenarioKind::Baseline => self.policy.to_string(),
            kind => format!("{}-{kind}", self.policy),
        };
        let path = dir.join(format!("{prefix}-seed-{:05}.json", self.seed));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json + "\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(path)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("invalid repro {}: {e}", path.display()))
    }
}
