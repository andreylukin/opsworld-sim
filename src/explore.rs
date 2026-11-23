//! Runs many seeded scenarios and aggregates the results.

use crate::agent::Policy;
use crate::invariants::Invariant;
use crate::repro::Repro;
use crate::scenario::ScenarioKind;
use crate::sim::{run_seed_in, RunResult};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOn {
    Never,
    Any,
    Invariant(Invariant),
}

impl StopOn {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "any" => Some(Self::Any),
            other => Invariant::parse(other).map(Self::Invariant),
        }
    }

    fn matches(self, result: &RunResult) -> Option<Invariant> {
        let first = result.violations.first()?.invariant;
        match self {
            Self::Never => None,
            Self::Any => Some(first),
            Self::Invariant(target) => result
                .violations
                .iter()
                .any(|v| v.invariant == target)
                .then_some(target),
        }
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub runs: u64,
    pub failed: u64,
    pub faults_injected: u64,
    pub by_invariant: BTreeMap<Invariant, u64>,
    pub stopped: Option<(Repro, PathBuf)>,
    pub first_repro: Option<PathBuf>,
    /// Total faults across failing schedules, before and after shrinking.
    pub faults_before_shrink: u64,
    pub faults_after_shrink: u64,
    /// The first repro with the largest reduction, shown as the example.
    pub best_shrink: Option<Repro>,
}

/// Explores `runs` seeds from `start`. `on_run` is called with each outcome.
pub fn explore(
    scenario: ScenarioKind,
    start: u64,
    runs: u64,
    policy: Policy,
    stop_on: StopOn,
    out: &Path,
    mut on_run: impl FnMut(u64, bool),
) -> Result<Summary, String> {
    let mut summary = Summary::default();
    for seed in start..start + runs {
        let result = run_seed_in(scenario, seed, policy);
        summary.runs += 1;
        summary.faults_injected += result.faults.len() as u64;
        on_run(summary.runs, result.passed());
        if result.passed() {
            continue;
        }
        summary.failed += 1;
        for v in &result.violations {
            *summary.by_invariant.entry(v.invariant).or_insert(0) += 1;
        }
        let stop_target = stop_on.matches(&result);
        let target = stop_target.unwrap_or(result.violations[0].invariant);
        let repro = Repro::from_failure(&result, policy, target);
        let path = repro.save(out)?;
        summary.faults_before_shrink += repro.faults_before.len() as u64;
        summary.faults_after_shrink += repro.faults.len() as u64;
        let removed = |r: &Repro| r.faults_before.len() - r.faults.len();
        if summary
            .best_shrink
            .as_ref()
            .is_none_or(|best| removed(&repro) > removed(best))
        {
            summary.best_shrink = Some(repro.clone());
        }
        summary.first_repro.get_or_insert_with(|| path.clone());
        if stop_target.is_some() {
            summary.stopped = Some((repro, path));
            break;
        }
    }
    Ok(summary)
}
