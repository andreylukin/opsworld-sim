//! The simulation loop and the intercepting tool port.

use crate::agent::{Agent, Policy};
use crate::faults::{build_feed, generate_schedule, Fault};
use crate::invariants::{Oracle, Violation};
use crate::model::{clock, Minutes, ShipmentId};
use crate::scenario::{Scenario, ScenarioKind};
use crate::tools::{ToolCall, ToolError, ToolPort, ToolReply, ToolResult, WriteKind};
use crate::world::World;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Deliver,
    Fault,
    Read,
    Write,
    Rejected,
    Escalate,
    Violation,
    End,
}

impl EventKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Deliver => "DOC",
            Self::Fault => "FAULT",
            Self::Read => "read",
            Self::Write => "WRITE",
            Self::Rejected => "REJECT",
            Self::Escalate => "ESCALATE",
            Self::Violation => "VIOLATION",
            Self::End => "END",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    pub step: u32,
    pub at: Minutes,
    pub kind: EventKind,
    pub shipment: Option<ShipmentId>,
    pub text: String,
}

impl TraceEvent {
    /// The canonical form. This is what the trace hash covers.
    pub fn plain(&self) -> String {
        format!(
            "{:04} {} {:<9} {}",
            self.step,
            clock(self.at),
            self.kind.tag(),
            self.text
        )
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub scenario: ScenarioKind,
    pub seed: u64,
    pub policy: String,
    pub faults: Vec<Fault>,
    pub trace: Vec<TraceEvent>,
    pub hash: String,
    pub violations: Vec<Violation>,
    /// Step at which the first violation fired.
    pub violation_step: Option<u32>,
}

impl RunResult {
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }
}

pub fn trace_hash(trace: &[TraceEvent]) -> String {
    let mut hasher = Sha256::new();
    for event in trace {
        hasher.update(event.plain().as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TimeoutMode {
    AfterCommit,
    BeforeCommit,
}

/// Decides which write calls time out, by tool and occurrence.
struct ToolFaults {
    plan: Vec<(WriteKind, u32, TimeoutMode)>,
    seen: BTreeMap<WriteKind, u32>,
}

impl ToolFaults {
    fn new(faults: &[Fault]) -> Self {
        let plan = faults
            .iter()
            .filter_map(|f| match f {
                Fault::TimeoutAfterCommit { tool, nth } => {
                    Some((*tool, *nth, TimeoutMode::AfterCommit))
                }
                Fault::TimeoutBeforeCommit { tool, nth } => {
                    Some((*tool, *nth, TimeoutMode::BeforeCommit))
                }
                _ => None,
            })
            .collect();
        Self {
            plan,
            seen: BTreeMap::new(),
        }
    }

    fn on_write(&mut self, kind: WriteKind) -> Option<TimeoutMode> {
        let count = self.seen.entry(kind).or_insert(0);
        let nth = *count;
        *count += 1;
        self.plan
            .iter()
            .filter(|(k, n, _)| *k == kind && *n == nth)
            .map(|(_, _, mode)| *mode)
            .min()
    }
}

struct Sim {
    world: World,
    oracle: Oracle,
    tool_faults: ToolFaults,
    trace: Vec<TraceEvent>,
    violations: Vec<Violation>,
    violation_step: Option<u32>,
    now: Minutes,
    step: u32,
}

impl Sim {
    fn log(&mut self, kind: EventKind, shipment: Option<ShipmentId>, text: String) {
        self.trace.push(TraceEvent {
            step: self.step,
            at: self.now,
            kind,
            shipment,
            text,
        });
    }

    fn fire(&mut self, found: Vec<Violation>) {
        if found.is_empty() || !self.violations.is_empty() {
            return;
        }
        self.violation_step = Some(self.step);
        for v in &found {
            let text = format!("{}: {}", v.invariant, v.detail);
            self.log(EventKind::Violation, v.shipment, text);
        }
        self.violations = found;
    }

    fn halted(&self) -> bool {
        !self.violations.is_empty()
    }
}

impl ToolPort for Sim {
    fn now(&self) -> Minutes {
        self.now
    }

    fn call(&mut self, call: ToolCall) -> ToolResult {
        if self.halted() {
            return Err(ToolError::Halted);
        }
        self.step += 1;
        let shipment = Some(call.shipment());
        let mut found = self.oracle.before_call(&call);
        let timeout = call.write_kind().and_then(|k| self.tool_faults.on_write(k));

        let result = match timeout {
            Some(TimeoutMode::BeforeCommit) => {
                self.log(
                    EventKind::Fault,
                    shipment,
                    format!("{call} => TIMEOUT (write was lost, agent cannot tell)"),
                );
                Err(ToolError::Timeout)
            }
            Some(TimeoutMode::AfterCommit) => {
                let note = match self.world.apply(&call) {
                    Ok((_, note)) => note,
                    Err(reason) => reason,
                };
                self.log(
                    EventKind::Fault,
                    shipment,
                    format!("{call} => TIMEOUT (write committed: {note}, agent cannot tell)"),
                );
                Err(ToolError::Timeout)
            }
            None => match self.world.apply(&call) {
                Ok((reply, note)) => {
                    let kind = match &call {
                        ToolCall::Escalate { .. } => EventKind::Escalate,
                        c if c.is_read() => EventKind::Read,
                        _ => EventKind::Write,
                    };
                    self.log(kind, shipment, format!("{call} => {note}"));
                    Ok::<ToolReply, ToolError>(reply)
                }
                Err(reason) => {
                    self.log(EventKind::Rejected, shipment, format!("{call} => {reason}"));
                    Err(ToolError::Rejected(reason))
                }
            },
        };

        self.oracle
            .record_outcome(&call, matches!(result, Err(ToolError::Timeout)));
        if call.is_business_write() {
            found.extend(self.oracle.after_write(&self.world));
        }
        self.fire(found);
        result
    }
}

/// Runs one scenario under a fault schedule.
pub fn run(scenario: &Scenario, faults: &[Fault], agent: &mut dyn Agent) -> RunResult {
    let feed = build_feed(scenario, faults);
    let mut sim = Sim {
        world: scenario.world.clone(),
        oracle: Oracle::new(feed.truth.clone()),
        tool_faults: ToolFaults::new(faults),
        trace: Vec::new(),
        violations: Vec::new(),
        violation_step: None,
        now: 0,
        step: 0,
    };

    for delivery in &feed.deliveries {
        sim.now = delivery.at;
        sim.step += 1;
        let shipment = Some(delivery.doc.shipment);
        for note in &delivery.notes {
            sim.log(EventKind::Fault, shipment, note.clone());
        }
        sim.log(EventKind::Deliver, shipment, delivery.doc.summary());
        sim.oracle.observe(delivery);
        agent.handle(&delivery.doc, &mut sim);
        if !sim.halted() {
            let found = sim.oracle.after_document(&sim.world, sim.now);
            sim.fire(found);
        }
        if sim.halted() {
            break;
        }
    }

    if !sim.halted() {
        sim.step += 1;
        let found = sim.oracle.at_end(&sim.world);
        sim.fire(found);
    }
    if !sim.halted() {
        sim.log(EventKind::End, None, "all invariants held".to_string());
    }

    let hash = trace_hash(&sim.trace);
    RunResult {
        scenario: scenario.kind,
        seed: scenario.seed,
        policy: agent.name().to_string(),
        faults: faults.to_vec(),
        trace: sim.trace,
        hash,
        violations: sim.violations,
        violation_step: sim.violation_step,
    }
}

/// Runs a seed under an explicit schedule.
pub fn run_with(seed: u64, faults: &[Fault], policy: Policy) -> RunResult {
    run_in(ScenarioKind::Baseline, seed, faults, policy)
}

/// Runs a seed of a named scenario under an explicit schedule.
pub fn run_in(kind: ScenarioKind, seed: u64, faults: &[Fault], policy: Policy) -> RunResult {
    let scenario = Scenario::build(kind, seed);
    let mut agent = policy.agent();
    run(&scenario, faults, agent.as_mut())
}

/// Runs a seed under the schedule that seed generates.
pub fn run_seed(seed: u64, policy: Policy) -> RunResult {
    run_seed_in(ScenarioKind::Baseline, seed, policy)
}

/// Runs a seed of a named scenario under the schedule that seed generates.
pub fn run_seed_in(kind: ScenarioKind, seed: u64, policy: Policy) -> RunResult {
    let scenario = Scenario::build(kind, seed);
    let faults = generate_schedule(&scenario);
    let mut agent = policy.agent();
    run(&scenario, &faults, agent.as_mut())
}
